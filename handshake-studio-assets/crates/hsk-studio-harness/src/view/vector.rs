//! Vector drawing and path editing in the viewer (lane SMB-ILLUSTRATOR-PEN slice 4): the pen,
//! direct-selection (node select) and shape tools, live shape parameters, the unified pathfinder,
//! offset path and outline stroke.
//!
//! Model: a vector layer is a Folio `vector` layer whose `Payload::Vector` names `SVPT` paths. The
//! path geometry is Nib's editable anchor model (`hsk_studio_nib::anchor_path::AnchorPath`) with a
//! fill / stroke style, kept with the document in the session document entry `vector`
//! (`studio/vector.<sha256>`, schema `hsk.harness.vector_paths@1`; held in memory until the
//! session carries that token, see [`entry_persistable`]), together with the Folio
//! `StudioLiveShapes` records of shapes whose parameters are still editable (STU-VEC-010). Paths
//! are append-only and content-addressed by a fresh `SVPT` id per edit, so every revision the
//! history can return to still resolves.
//!
//! Transactions: every edit is ONE chronicle transaction of `placement` writes through the session
//! (undoable as one step, re-validated by Folio). A new path or shape creates a vector layer above
//! the selected layer. An edit of an existing path (pen extension, anchor or handle drag, segment
//! bend, anchor convert / delete / insert, scissors, join, reverse, live parameter change, expand,
//! outline stroke) is one chronicle `vector_paths` write of the layer's new path id (lane
//! SMB-ILLUSTRATOR-PEN-2): the layer keeps its id. Pathfinder
//! creates its result layers above the topmost operand and deletes the operands, in one
//! transaction; offset path creates a copy (STU-VEC-006 "numeric inset/outset copy").
//!
//! Rendering (lane SMB-VECTOR-STYLES): every vector layer is prepared by composite's Folio adapter
//! ([`hsk_studio_composite::prepare_vectors`]: nib exact coverage, nib strokes, networks, region
//! fill styles, width profiles) from the entry's Folio records: its paint styles ([`style`]; an
//! unstyled layer through a style synthesised from its 8-bit colours) and its width profiles
//! (Folio `StudioWidthProfile`, entry member `vector_records`); then lowered with
//! `evaluate_with_inputs` / `lower_with_inputs` ([`super::render::render_full`]). Colours are 8-bit
//! sRGB in the entry and linear light in the plan (the plan's blend space, [`style::plan_colour`]).
//!
//! Studio choices (Illustrator parity UNVERIFIED, D5): new pen paths have no fill and a 2 px stroke
//! in the brush colour; shapes are filled with the brush colour and have no stroke; the pen closes
//! a path when the click lands within the hit radius of its first anchor; a direct edit of a live
//! shape expands it (its live record no longer applies to the new path).
#![deny(clippy::arithmetic_side_effects)]
use super::Viewer;
use super::edit::Tool;
use crate::{Place, PlacementWrite};
use hsk_studio_composite::{PreparedVectors, VectorResolution};
use hsk_studio_folio::{self as folio, GeometryUnit, Length, Parent, Payload, StudioDocument, StudioLiveShape, StudioLiveShapes, StudioShapeParameters};
use hsk_studio_nib::anchor_path::{
    self as ap, AnchorAddr, AnchorPath, AnchorSelection, Constraint, Contour, EditAnchor, EndAddr, HandleSide,
};
use hsk_studio_nib::boolean::BooleanOptions;
use hsk_studio_nib::freeform::{self, CornerTreatment};
use hsk_studio_nib::network::{NetworkRegion, NetworkSegment, NetworkVertex, VectorNetwork};
use hsk_studio_nib::width::{WidthPoint, WidthProfile};
use hsk_studio_nib::OwnedPath;
use hsk_studio_nib::pathfinder::{PathfinderOp, pathfinder};
use hsk_studio_nib::shape::{self, CornerKind, EllipseSpec, RectSpec};
use hsk_studio_nib::stroke::{LineJoin, StrokeOptions, StrokeStyle, outline_stroke};
use hsk_studio_nib::{FillRule, Mirroring, PathPoint};
use serde_json::{Value, json};

/// Session document entry token of the vector records.
pub const ENTRY: &str = "vector";
/// Schema of the entry this module writes.
pub const ENTRY_SCHEMA: &str = "hsk.harness.vector_paths@1";
/// Most path records one entry holds (every edit appends one).
pub const MAX_PATHS: usize = 4096;
/// Most anchors over all path records of one entry (checked before decoding a path).
pub const MAX_ENTRY_ANCHORS: usize = 1 << 20;
/// Largest stroke width (document pixels).
pub const MAX_STROKE_WIDTH: f64 = 1000.0;
/// Default hit radius of the canvas gestures (screen pixels; divided by the zoom).
pub const HIT_RADIUS_SCREEN: f64 = 6.0;

pub mod style;
pub use style::{GradientStop, PaintSpec, RowKind, StrokePanel, SwatchLink};

/// Stable widget ids of the vector tools.
pub mod widget {
    pub const FILL: &str = "vector.fill";
    pub const FILL_ON: &str = "vector.fill.on";
    pub const STROKE: &str = "vector.stroke";
    pub const STROKE_ON: &str = "vector.stroke.on";
    pub const STROKE_WIDTH: &str = "vector.stroke.width";
    pub const CORNER_RADIUS: &str = "vector.shape.corner_radius";
    pub const SIDES: &str = "vector.shape.sides";
    pub const POINTS: &str = "vector.shape.points";
    pub const INNER: &str = "vector.shape.inner_radius";
    pub const PEN_END: &str = "vector.pen.end";
    pub const PATHFINDER: &str = "vector.pathfinder";
    /// Lane SMB-ILLUSTRATOR-PEN-2.
    pub const PENCIL_FIDELITY: &str = "vector.pencil.fidelity";
    pub const ARC_SLOPE: &str = "vector.arc.slope";
    pub const ARC_CLOSED: &str = "vector.arc.closed";
    pub const GRID_ROWS: &str = "vector.grid.rows";
    pub const GRID_COLUMNS: &str = "vector.grid.columns";
    pub const POLAR_CONCENTRIC: &str = "vector.polar.concentric";
    pub const POLAR_RADIAL: &str = "vector.polar.radial";
    pub const SPIRAL_DECAY: &str = "vector.spiral.decay";
    pub const SPIRAL_TURNS: &str = "vector.spiral.turns";
    /// Lane SMB-ILLUSTRATOR-PEN-3.
    pub const RESHAPE_RADIUS: &str = "vector.reshape.radius";
    pub const BLOB_DIAMETER: &str = "vector.blob.diameter";
}

/// Paint of one path: 8-bit sRGB fill and stroke (absent = none) and the stroke width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VectorStyle {
    pub fill: Option<[u8; 3]>,
    pub stroke: Option<[u8; 3]>,
    pub stroke_width: f64,
}

/// One path record of the entry.
#[derive(Clone, Debug, PartialEq)]
pub struct PathRecord {
    pub path_id: String,
    pub path: AnchorPath,
    pub style: VectorStyle,
    /// The Folio `StudioVectorNetwork` record when the path is a network (lane SMB-ILLUSTRATOR-PEN-3;
    /// `path` is then its stroke geometry, for consumers that ignore networks).
    pub network: Option<folio::StudioVectorNetwork>,
    /// Width profile `[t, width]` of a variable-width stroke (lane SMB-ILLUSTRATOR-PEN-3).
    pub width: Option<Vec<[f64; 2]>>,
}

impl PathRecord {
    /// A record without network and width profile.
    pub fn plain(path_id: String, path: AnchorPath, style: VectorStyle) -> Self {
        Self { path_id, path, style, network: None, width: None }
    }
}

/// Network edits of the model surface ([`Viewer::edit_network`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NetworkEdit {
    MoveVertex { vertex: usize, dx: f64, dy: f64 },
    AddVertex { x: f64, y: f64 },
    Connect { a: usize, b: usize },
    DeleteSegment(usize),
    Tangents { segment: usize, start: Option<[f64; 2]>, end: Option<[f64; 2]> },
}

/// Planarize tolerance of the paint bucket (document px).
pub const PLANAR_TOLERANCE: f64 = 1.0e-6;

/// Nib network of a Folio network record.
pub fn network_of(n: &folio::StudioVectorNetwork) -> Result<VectorNetwork, String> {
    hsk_studio_composite::vector::nib_network(n).map_err(|e| format!("network: {}", e.code()))
}

/// Folio network record of a nib network (indices saturate at `u32::MAX`, refused by validation).
pub fn folio_of(n: &VectorNetwork, path_id: &str) -> folio::StudioVectorNetwork {
    use hsk_studio_nib::stroke::{LineCap, LineJoin};
    let idx = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
    folio::StudioVectorNetwork {
        schema_id: folio::VectorNetworkSchema::V1,
        path_id: path_id.to_string(),
        vertices: n
            .vertices
            .iter()
            .map(|v| folio::StudioNetworkVertex {
                x: v.x,
                y: v.y,
                stroke_cap: v.stroke_cap.map(|c| match c {
                    LineCap::Butt => folio::StudioStrokeCap::Butt,
                    LineCap::Round => folio::StudioStrokeCap::Round,
                    LineCap::Square => folio::StudioStrokeCap::Projecting,
                }),
                stroke_join: v.stroke_join.map(|j| match j {
                    LineJoin::Miter => folio::StudioStrokeJoin::Miter,
                    LineJoin::Round => folio::StudioStrokeJoin::Round,
                    LineJoin::Bevel => folio::StudioStrokeJoin::Bevel,
                }),
                corner_radius: v.corner_radius,
                handle_mirroring: v.handle_mirroring.map(|m| match m {
                    Mirroring::None => folio::StudioHandleMirroring::None,
                    Mirroring::Angle => folio::StudioHandleMirroring::Angle,
                    Mirroring::AngleAndLength => folio::StudioHandleMirroring::AngleAndLength,
                }),
            })
            .collect(),
        segments: n
            .segments
            .iter()
            .map(|s| folio::StudioNetworkSegment { start: idx(s.start), end: idx(s.end), tangent_start: s.tangent_start.map(|t| [t.x, t.y]), tangent_end: s.tangent_end.map(|t| [t.x, t.y]) })
            .collect(),
        regions: n
            .regions
            .iter()
            .map(|r| folio::StudioNetworkRegion {
                winding_rule: if r.winding_rule == FillRule::EvenOdd { folio::StudioRegionWinding::EvenOdd } else { folio::StudioRegionWinding::NonZero },
                loops: r.loops.iter().map(|l| l.iter().map(|k| idx(*k)).collect()).collect(),
                fills: r.fills,
                fill_style_id: r.fill_style_id.clone(),
            })
            .collect(),
    }
}

fn width_profile(w: &[[f64; 2]]) -> Result<WidthProfile, String> {
    let p = WidthProfile { points: w.iter().map(|p| WidthPoint { t: p[0], width: p[1] }).collect() };
    p.validate().map_err(|e| format!("width_profile: {e:?}"))?;
    Ok(p)
}

/// Point-in-region test on the flattened outline of an anchor path (non-zero / even-odd per path).
fn region_contains(path: &AnchorPath, q: PathPoint) -> bool {
    let Ok(owned) = path.to_owned_path() else { return false };
    let Ok(lines) = hsk_studio_nib::flatten::flatten(&owned, &hsk_studio_nib::flatten::FlattenOptions::default()) else { return false };
    let mut polygon = OwnedPath::new(owned.fill_rule);
    for l in lines {
        let mut it = l.points.iter();
        let Some(first) = it.next() else { continue };
        polygon.elements.push(hsk_studio_nib::PathElement::MoveTo(*first));
        polygon.elements.extend(it.map(|p| hsk_studio_nib::PathElement::LineTo(*p)));
        polygon.elements.push(hsk_studio_nib::PathElement::Close);
    }
    freeform::polygon_contains(&polygon, q)
}

/// A point inside a closed anchor path: the area centroid of its flattened outline when inside,
/// else the midpoint of the first inside span of the horizontal line through the centroid.
fn interior_point(path: &AnchorPath) -> Option<PathPoint> {
    let owned = path.to_owned_path().ok()?;
    let lines = hsk_studio_nib::flatten::flatten(&owned, &hsk_studio_nib::flatten::FlattenOptions::default()).ok()?;
    let pts: Vec<PathPoint> = lines.iter().flat_map(|l| l.points.iter().copied()).collect();
    let (mut a, mut cx, mut cy) = (0.0, 0.0, 0.0);
    for l in &lines {
        let n = l.points.len();
        for i in 0..n {
            let (p, q) = (l.points[i], l.points[i.saturating_add(1).checked_rem(n)?]);
            let cross = p.x * q.y - q.x * p.y;
            a += cross;
            cx += (p.x + q.x) * cross;
            cy += (p.y + q.y) * cross;
        }
    }
    if a == 0.0 || pts.is_empty() {
        return None;
    }
    let c = PathPoint::new(cx / (3.0 * a), cy / (3.0 * a));
    if region_contains(path, c) {
        return Some(c);
    }
    let mut xs: Vec<f64> = Vec::new();
    for l in &lines {
        let n = l.points.len();
        for i in 0..n {
            let (p, q) = (l.points[i], l.points[i.saturating_add(1).checked_rem(n)?]);
            if (p.y <= c.y) != (q.y <= c.y) {
                xs.push(p.x + (c.y - p.y) / (q.y - p.y) * (q.x - p.x));
            }
        }
    }
    xs.sort_by(f64::total_cmp);
    xs.windows(2).map(|w| PathPoint::new((w[0] + w[1]) / 2.0, c.y)).find(|m| region_contains(path, *m))
}

/// Arc-length fraction (0..=1) of the point at `(segment, t)` of contour `contour` (16 samples per segment).
fn arc_fraction(path: &AnchorPath, contour: usize, segment: usize, t: f64) -> Result<f64, String> {
    let c = path.contour(contour).map_err(|e| e.code().to_string())?;
    let at = |si: usize, tk: f64| ap::point_at(path, contour, si, tk).map_err(|e| e.code().to_string());
    let dist = |a: PathPoint, b: PathPoint| ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
    let (mut total, mut upto) = (0.0, 0.0);
    for si in 0..c.segment_count() {
        let mut prev = at(si, 0.0)?;
        for k in 1..=16_u32 {
            let tk = f64::from(k) / 16.0;
            let q = at(si, tk)?;
            let step = dist(prev, q);
            total += step;
            if si < segment {
                upto += step;
            } else if si == segment {
                let t0 = f64::from(k.saturating_sub(1)) / 16.0;
                if tk <= t {
                    upto += step;
                } else if t0 < t {
                    // The partial step up to the grabbed point.
                    upto += dist(prev, at(si, t)?);
                }
            }
            prev = q;
        }
    }
    Ok(if total > 0.0 { (upto / total).clamp(0.0, 1.0) } else { 0.0 })
}

/// The `vector` document entry.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorEntry {
    pub paths: Vec<PathRecord>,
    /// Live shape records (Folio `StudioLiveShape`); only those whose layer and path are in the
    /// document are validated and offered (the others belong to revisions the history can return to).
    pub live: Vec<StudioLiveShape>,
    /// Folio paint styles of styled layers and swatches (lane SMB-VECTOR-STYLES, [`style`]; entry
    /// member `design`, absent when `None`).
    pub design: Option<folio::StudioDesignRecords>,
    /// Swatch links (entry member `swatch_links`, absent when empty).
    pub links: Vec<SwatchLink>,
}

/// Shape tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    Rectangle,
    RoundedRectangle,
    Ellipse,
    Polygon,
    Star,
    Line,
    /// Lane SMB-ILLUSTRATOR-PEN-2.
    ArcSegment,
    RectangularGrid,
    PolarGrid,
    Spiral,
}

/// A path edit of the model surface ([`Viewer::edit_path`]); each is one transaction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathEdit {
    InsertAnchor { contour: usize, segment: usize, t: f64 },
    DeleteAnchor(AnchorAddr),
    Convert(AnchorAddr, Mirroring),
    MoveAnchor { at: AnchorAddr, dx: f64, dy: f64, constraint: Constraint },
    MoveHandle { at: AnchorAddr, side: HandleSide, to: [f64; 2], constraint: Constraint, break_mirroring: bool },
    Bend { contour: usize, segment: usize, t: f64, dx: f64, dy: f64 },
    SplitAt { contour: usize, segment: usize, t: f64 },
    SplitAtAnchor(AnchorAddr),
    Join(EndAddr, EndAddr),
    Reverse(Option<usize>),
    /// Live corner on an anchor with two incident segments (round, inverted round, chamfer).
    Corner { at: AnchorAddr, kind: CornerTreatment, radius: f64 },
}

/// What a direct-selection hit found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DirectTarget {
    Anchor(AnchorAddr),
    Handle(AnchorAddr, HandleSide),
    Segment { contour: usize, segment: usize, t: f64 },
}

/// Vector tool settings and the gesture in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorTool {
    pub fill_on: bool,
    pub fill: [u8; 3],
    pub stroke_on: bool,
    pub stroke: [u8; 3],
    pub stroke_width: f64,
    pub corner_radius: f64,
    pub polygon_sides: u32,
    pub star_points: u32,
    pub star_inner: f64,
    /// Arc segment slope (percent), closed (pie) and base axis.
    pub arc_slope: f64,
    pub arc_closed: bool,
    /// Grid dividers (rectangular: rows, columns; polar: concentric, radial).
    pub grid_rows: u32,
    pub grid_columns: u32,
    pub polar_concentric: u32,
    pub polar_radial: u32,
    /// Spiral decay (percent per turn) and turns.
    pub spiral_decay: f64,
    pub spiral_turns: f64,
    /// Pencil fidelity 0..=4 (STU-VEC-119 soft range): RDP tolerance 0.5 * 2^fidelity px; ends
    /// closer than `pencil_close_within` px close the path (0 = never).
    pub pencil_fidelity: u32,
    pub pencil_close_within: f64,
    /// Live corner treatment and radius of the corner command.
    pub corner: CornerTreatment,
    /// Kerf of the knife (document px).
    pub knife_kerf: f64,
    /// Reshape falloff radius and blob brush diameter (document px).
    pub reshape_radius: f64,
    pub blob_diameter: f64,
    /// The freehand samples of the gesture in progress (pencil, knife, shape builder).
    pub samples: Vec<[f64; 2]>,
    /// The layer the pen extends (`None` = the next click starts a new path).
    pub pen_layer: Option<String>,
    /// The network vertex the pen continues from on a network layer.
    pub pen_vertex: Option<usize>,
    /// Press point and current point of the canvas gesture in progress (document pixels;
    /// [`Viewer::set_vector_tool`] keeps the live values).
    pub press: Option<[f64; 2]>,
    pub current: Option<[f64; 2]>,
}

impl Default for VectorTool {
    fn default() -> Self {
        Self {
            fill_on: true,
            fill: [220, 30, 40],
            stroke_on: true,
            stroke: [0, 0, 0],
            stroke_width: 2.0,
            corner_radius: 8.0,
            polygon_sides: 6,
            star_points: 5,
            star_inner: 0.5,
            arc_slope: 0.0,
            arc_closed: false,
            grid_rows: 5,
            grid_columns: 5,
            polar_concentric: 5,
            polar_radial: 6,
            spiral_decay: 80.0,
            spiral_turns: 2.5,
            pencil_fidelity: 2,
            pencil_close_within: 4.0,
            corner: CornerTreatment::Round,
            knife_kerf: 0.01,
            reshape_radius: 40.0,
            blob_diameter: 10.0,
            samples: Vec::new(),
            pen_layer: None,
            pen_vertex: None,
            press: None,
            current: None,
        }
    }
}

// ----- entry wire form --------------------------------------------------------------------------

fn rgb_json(c: Option<[u8; 3]>) -> Value {
    c.map_or(Value::Null, |c| json!(c))
}

fn rgb_of(v: &Value, field: &str) -> Result<Option<[u8; 3]>, String> {
    if v.is_null() {
        return Ok(None);
    }
    let a = v.as_array().filter(|a| a.len() == 3).ok_or_else(|| format!("vector_entry_style: {field}"))?;
    let mut out = [0_u8; 3];
    for (o, x) in out.iter_mut().zip(a) {
        *o = x.as_u64().and_then(|n| u8::try_from(n).ok()).ok_or_else(|| format!("vector_entry_style: {field}"))?;
    }
    Ok(Some(out))
}

fn mirroring_wire(m: Mirroring) -> &'static str {
    m.wire()
}

fn mirroring_of(text: &str) -> Result<Mirroring, String> {
    match text {
        "NONE" => Ok(Mirroring::None),
        "ANGLE" => Ok(Mirroring::Angle),
        "ANGLE_AND_LENGTH" => Ok(Mirroring::AngleAndLength),
        _ => Err("vector_entry_mirroring".into()),
    }
}

fn path_json(p: &AnchorPath) -> Value {
    let contours: Vec<Value> = p
        .contours
        .iter()
        .map(|c| {
            let anchors: Vec<Value> = c
                .anchors
                .iter()
                .map(|a| json!([a.position.x, a.position.y, a.incoming.x, a.incoming.y, a.outgoing.x, a.outgoing.y, mirroring_wire(a.mirroring)]))
                .collect();
            json!({"closed": c.closed, "anchors": anchors})
        })
        .collect();
    json!({"fill_rule": p.fill_rule.wire(), "contours": contours})
}

/// Decodes one path; `budget` is the anchor allowance left for the entry (charged before any anchor
/// is allocated).
fn path_of(v: &Value, budget: &mut usize) -> Result<AnchorPath, String> {
    let fill_rule = FillRule::parse(v["fill_rule"].as_str().ok_or("vector_entry_fill_rule")?).map_err(|_| "vector_entry_fill_rule")?;
    let contours = v["contours"].as_array().ok_or("vector_entry_contours")?;
    if contours.len() > ap::MAX_CONTOURS {
        return Err("vector_entry_over_budget".into());
    }
    let mut out = AnchorPath::new(fill_rule);
    for c in contours {
        let anchors = c["anchors"].as_array().ok_or("vector_entry_anchors")?;
        *budget = budget.checked_sub(anchors.len()).ok_or("vector_entry_over_budget")?;
        let mut list = Vec::with_capacity(anchors.len());
        for a in anchors {
            let a = a.as_array().filter(|a| a.len() == 7).ok_or("vector_entry_anchor")?;
            let n = |i: usize| a.get(i).and_then(Value::as_f64).ok_or("vector_entry_anchor");
            list.push(EditAnchor {
                position: PathPoint::new(n(0)?, n(1)?),
                incoming: PathPoint::new(n(2)?, n(3)?),
                outgoing: PathPoint::new(n(4)?, n(5)?),
                mirroring: mirroring_of(a.get(6).and_then(Value::as_str).ok_or("vector_entry_anchor")?)?,
            });
        }
        out.contours.push(Contour { anchors: list, closed: c["closed"].as_bool().ok_or("vector_entry_closed")? });
    }
    out.validate().map_err(|e| format!("vector_entry_path: {}", e.code()))?;
    Ok(out)
}

impl VectorEntry {
    pub fn empty() -> Self {
        Self { paths: Vec::new(), live: Vec::new(), design: None, links: Vec::new() }
    }

    pub fn path(&self, path_id: &str) -> Option<&PathRecord> {
        self.paths.iter().find(|p| p.path_id == path_id)
    }

    /// Strict decode: known fields only, schema, budgets (paths, anchors) before allocation, every
    /// path validated by Nib, styles in range; then [`VectorEntry::validate`].
    pub fn decode(bytes: &[u8], doc: &StudioDocument, cancel: &hsk_studio_accord::CancellationToken) -> Result<Self, String> {
        let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("vector_entry_json: {e}"))?;
        let object = v.as_object().ok_or("vector_entry_json: not an object")?;
        if object.keys().any(|k| !["schema", "paths", "live_shapes", "design", "swatch_links", "vector_records"].contains(&k.as_str())) {
            return Err("vector_entry_unknown_field".into());
        }
        if v["schema"] != ENTRY_SCHEMA {
            return Err("vector_entry_schema".into());
        }
        let paths = v["paths"].as_array().ok_or("vector_entry_paths")?;
        if paths.len() > MAX_PATHS {
            return Err("vector_entry_over_budget".into());
        }
        let mut budget = MAX_ENTRY_ANCHORS;
        let mut records = Vec::with_capacity(paths.len());
        for p in paths {
            let o = p.as_object().ok_or("vector_entry_path")?;
            if o.keys().any(|k| !["path_id", "fill", "stroke", "stroke_width", "path", "network", "width"].contains(&k.as_str())) {
                return Err("vector_entry_unknown_field".into());
            }
            let path_id = p["path_id"].as_str().ok_or("vector_entry_path_id")?.to_string();
            let network = match o.get("network") {
                None => None,
                Some(n) => {
                    let n: folio::StudioVectorNetwork = serde_json::from_value(n.clone()).map_err(|e| format!("vector_entry_network: {e}"))?;
                    folio::validate_vector_network(&n).map_err(|d| format!("vector_entry_network: {:?} {}", d.code, d.target))?;
                    if n.path_id != path_id {
                        return Err(format!("vector_entry_network: path id {}", n.path_id));
                    }
                    Some(n)
                }
            };
            let width = match o.get("width") {
                None => None,
                Some(w) => {
                    let arr = w.as_array().filter(|a| a.len() <= hsk_studio_nib::width::MAX_WIDTH_POINTS).ok_or("vector_entry_width")?;
                    let pts = arr
                        .iter()
                        .map(|q| {
                            let q = q.as_array().filter(|q| q.len() == 2).ok_or("vector_entry_width")?;
                            Ok([q[0].as_f64().ok_or("vector_entry_width")?, q[1].as_f64().ok_or("vector_entry_width")?])
                        })
                        .collect::<Result<Vec<[f64; 2]>, String>>()?;
                    width_profile(&pts)?;
                    Some(pts)
                }
            };
            records.push(PathRecord {
                path_id,
                style: VectorStyle { fill: rgb_of(&p["fill"], "fill")?, stroke: rgb_of(&p["stroke"], "stroke")?, stroke_width: p["stroke_width"].as_f64().ok_or("vector_entry_style: stroke_width")? },
                path: path_of(&p["path"], &mut budget)?,
                network,
                width,
            });
        }
        let live: StudioLiveShapes = serde_json::from_value(v["live_shapes"].clone()).map_err(|e| format!("vector_entry_live_shapes: {e}"))?;
        if live.document_id != doc.document_id {
            return Err("vector_entry_document".into());
        }
        let design = match object.get("design") {
            None => None,
            Some(d) => {
                if ["nodes", "paint_styles"].iter().any(|m| d.get(m).and_then(Value::as_array).is_some_and(|a| a.len() > style::MAX_STYLES)) {
                    return Err("vector_entry_design: members".into());
                }
                Some(serde_json::from_value::<folio::StudioDesignRecords>(d.clone()).map_err(|e| format!("vector_entry_design: {e}"))?)
            }
        };
        let links = match object.get("swatch_links") {
            None => Vec::new(),
            Some(l) => {
                let arr = l.as_array().filter(|a| a.len() <= style::MAX_LINKS).ok_or("vector_entry_swatch_links")?;
                arr.iter()
                    .map(|x| {
                        let o = x.as_object().filter(|o| o.len() == 3).ok_or("vector_entry_swatch_links")?;
                        let text = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| "vector_entry_swatch_links".to_string());
                        let kind = RowKind::from_token(&text("kind")?).ok_or("vector_entry_swatch_links")?;
                        Ok(SwatchLink { layer_id: text("layer_id")?, kind, swatch_id: text("swatch_id")? })
                    })
                    .collect::<Result<Vec<_>, String>>()?
            }
        };
        style::validate_entry_styles(design.as_ref(), &links, &doc.document_id)?;
        if let Some(r) = object.get("vector_records") {
            if r.get("width_profiles").and_then(Value::as_array).is_some_and(|a| a.len() > MAX_PATHS) {
                return Err("vector_entry_over_budget".into());
            }
            let r: folio::StudioVectorRecords = serde_json::from_value(r.clone()).map_err(|e| format!("vector_entry_vector_records: {e}"))?;
            let mut rest = r.clone();
            rest.width_profiles.clear();
            if rest != folio::StudioVectorRecords::new(doc.document_id.clone()) {
                return Err("vector_entry_vector_records: members".into());
            }
            for w in &r.width_profiles {
                folio::validate_width_profile(w).map_err(|d| format!("vector_entry_width: {:?} {}", d.code, d.target))?;
                if !w.is_symmetric() || w.adjust_adjoining_width_points || w.single_width_only_incoming || w.single_width_only_outgoing {
                    return Err(format!("vector_entry_width: {} not editable here (independent widths or editing flags)", w.path_id));
                }
                let pts: Vec<[f64; 2]> = w.points.iter().map(|q| [q.position, q.width]).collect();
                width_profile(&pts)?;
                let record = records.iter_mut().find(|p| p.path_id == w.path_id).ok_or_else(|| format!("vector_entry_width: path {}", w.path_id))?;
                record.width = Some(pts);
            }
        }
        let entry = Self { paths: records, live: live.shapes, design, links };
        entry.validate(doc, cancel)?;
        Ok(entry)
    }

    /// Path ids are `SVPT` ids and unique; styles finite and bounded; the live records that apply
    /// to the document (layer present, path among its paths) pass Folio `validate_live_shapes`.
    pub fn validate(&self, doc: &StudioDocument, cancel: &hsk_studio_accord::CancellationToken) -> Result<(), String> {
        if self.paths.len() > MAX_PATHS || self.live.len() > MAX_PATHS {
            return Err("vector_entry_over_budget".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for p in &self.paths {
            let id = hsk_studio_accord::DomainId::parse(&p.path_id).map_err(|_| format!("vector_entry_path_id: {}", p.path_id))?;
            if id.prefix() != "SVPT" || !seen.insert(p.path_id.as_str()) {
                return Err(format!("vector_entry_path_id: {}", p.path_id));
            }
            let w = p.style.stroke_width;
            if !(w.is_finite() && (0.0..=MAX_STROKE_WIDTH).contains(&w)) {
                return Err(format!("vector_entry_style: stroke_width {}", p.path_id));
            }
            p.path.validate().map_err(|e| format!("vector_entry_path: {} {}", p.path_id, e.code()))?;
        }
        let applicable = self.applicable_live(doc);
        folio::validate_live_shapes(&applicable, doc, folio::Budget::default(), cancel).map_err(|d| format!("vector_live_shapes: {:?} {}", d.code, d.target))
    }

    /// The live records of layers the document holds with the recorded path.
    pub fn applicable_live(&self, doc: &StudioDocument) -> StudioLiveShapes {
        let mut set = StudioLiveShapes::new(&doc.document_id);
        set.shapes = self
            .live
            .iter()
            .filter(|s| doc.layers.iter().any(|l| l.layer_id == s.layer_id && matches!(&l.payload, Payload::Vector { path_ids } if path_ids.contains(&s.path_id))))
            .cloned()
            .collect();
        set
    }

    pub fn to_bytes(&self, document_id: &str) -> Result<Vec<u8>, String> {
        let paths: Vec<Value> = self
            .paths
            .iter()
            .map(|p| {
                let mut v = json!({"path_id": p.path_id, "fill": rgb_json(p.style.fill), "stroke": rgb_json(p.style.stroke), "stroke_width": p.style.stroke_width, "path": path_json(&p.path)});
                if let Some(n) = &p.network {
                    v["network"] = serde_json::to_value(n).unwrap_or(Value::Null);
                }
                v
            })
            .collect();
        let mut live = StudioLiveShapes::new(document_id);
        live.shapes.clone_from(&self.live);
        let mut v = json!({"schema": ENTRY_SCHEMA, "paths": paths, "live_shapes": live});
        let records = self.folio_records(document_id);
        if !records.width_profiles.is_empty() {
            v["vector_records"] = serde_json::to_value(&records).map_err(|e| format!("vector_entry_json: {e}"))?;
        }
        if let Some(d) = &self.design {
            v["design"] = serde_json::to_value(d).map_err(|e| format!("vector_entry_json: {e}"))?;
        }
        if !self.links.is_empty() {
            v["swatch_links"] = Value::Array(self.links.iter().map(|l| json!({"layer_id": l.layer_id, "kind": l.kind.token(), "swatch_id": l.swatch_id})).collect());
        }
        serde_json::to_vec(&v).map_err(|e| format!("vector_entry_json: {e}"))
    }

    /// The Folio vector records of the entry: a `StudioWidthProfile` per path with a width profile
    /// (lane SMB-VECTOR-STYLES; composite strokes them with nib's variable outline).
    pub fn folio_records(&self, document_id: &str) -> folio::StudioVectorRecords {
        let mut r = folio::StudioVectorRecords::new(document_id.to_string());
        r.width_profiles = self
            .paths
            .iter()
            .filter_map(|p| {
                let w = p.width.as_ref()?;
                Some(folio::StudioWidthProfile {
                    path_id: p.path_id.clone(),
                    points: w.iter().map(|q| folio::StudioWidthPoint { position: q[0], width: q[1], width_out: None }).collect(),
                    adjust_adjoining_width_points: false,
                    single_width_only_incoming: false,
                    single_width_only_outgoing: false,
                })
            })
            .collect();
        r
    }
}

/// True when the session saves a document entry under the token `vector` (session
/// `DOCUMENT_ENTRY_TOKENS`). Until then the viewer keeps the entry in memory for the open document
/// (undo, redo and rendering work; [`Viewer::save`] refuses a document with vector layers, typed
/// `vector_entry_not_persistable`). SPEC/OWNERSHIP GAP: the token belongs to the session crate
/// (lead request in lane SMB-ILLUSTRATOR-PEN).
pub fn entry_persistable() -> bool {
    hsk_studio_session::DOCUMENT_ENTRY_TOKENS.contains(&ENTRY)
}

/// Composite's view of a saved vector entry: path geometry and network records by `SVPT` id
/// (no image paints).
struct EntryAssets<'a>(&'a VectorEntry);

impl hsk_studio_composite::VectorAssets for EntryAssets<'_> {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        self.0.path(path_id)?.path.to_owned_path().ok()
    }
    fn image(&self, _source: &folio::StudioImageSource) -> Option<hsk_studio_composite::ImageInfo> {
        None
    }
    fn network(&self, path_id: &str) -> Option<folio::StudioVectorNetwork> {
        self.0.path(path_id)?.network.clone()
    }
    fn plan_colour(&self, colour: &folio::StudioColor, space: hsk_studio_composite::BlendSpace) -> Option<[f32; 3]> {
        style::plan_colour(colour, space)
    }
}

/// A fresh `SVPT` id (the document id's time fields, never one the entry holds).
fn mint_path_id(doc: &StudioDocument, entry: &VectorEntry) -> String {
    let uuid = doc.document_id.split_once('-').map_or("", |(_, u)| u);
    let (high, low) = (uuid.get(0..8).unwrap_or("00000000"), uuid.get(9..13).unwrap_or("0000"));
    let mut n = u64::try_from(entry.paths.len()).unwrap_or(0);
    loop {
        let id = format!("SVPT-{high}-{low}-7ede-a000-{n:012x}");
        if entry.path(&id).is_none() {
            return id;
        }
        n = n.wrapping_add(1);
    }
}

fn length(unit: GeometryUnit, value: f64) -> Length {
    Length { value, unit }
}

/// The geometry and the live record of a shape dragged from `a` to `b` (`constrain` = Shift:
/// square / circle bounds, 45-degree line).
fn shape_of(kind: ShapeKind, tool: &VectorTool, a: [f64; 2], b: [f64; 2], constrain: bool, unit: GeometryUnit) -> Result<(AnchorPath, StudioShapeParameters), String> {
    let (mut w, mut h) = ((b[0] - a[0]).abs(), (b[1] - a[1]).abs());
    if constrain {
        let m = w.max(h);
        (w, h) = (m, m);
    }
    let x = if b[0] < a[0] { a[0] - w } else { a[0] };
    let y = if b[1] < a[1] { a[1] - h } else { a[1] };
    let refuse = |e: shape::ShapeError| format!("shape_{}: {}", e.parameter, e.error.code());
    let l = |v: f64| length(unit, v);
    match kind {
        ShapeKind::Rectangle | ShapeKind::RoundedRectangle => {
            let r = if kind == ShapeKind::Rectangle { 0.0 } else { tool.corner_radius };
            let corner_kind = if r > 0.0 { CornerKind::Round } else { CornerKind::None };
            let spec = RectSpec { x, y, width: w, height: h, corner_radius: [r; 4], corner_kind, corner_smoothing: 0.0 };
            let (path, applied) = shape::rectangle(&spec).map_err(refuse)?;
            let corner_kind = if corner_kind == CornerKind::Round { folio::StudioCornerKind::Round } else { folio::StudioCornerKind::None };
            Ok((path, StudioShapeParameters::Rectangle { x: l(x), y: l(y), width: l(w), height: l(h), corner_radius: applied.map(l), corner_kind, corner_smoothing: 0.0 }))
        }
        ShapeKind::Ellipse => {
            let (rx, ry) = (w / 2.0, h / 2.0);
            let spec = EllipseSpec::full(x + rx, y + ry, rx, ry);
            let path = shape::ellipse(&spec).map_err(refuse)?;
            Ok((path, StudioShapeParameters::Ellipse { center_x: l(spec.cx), center_y: l(spec.cy), radius_x: l(rx), radius_y: l(ry), start_angle: 0.0, end_angle: 360.0, inner_ratio: 0.0 }))
        }
        ShapeKind::Polygon | ShapeKind::Star => {
            let r = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            if kind == ShapeKind::Polygon {
                let path = shape::polygon(a[0], a[1], r, tool.polygon_sides, 0.0).map_err(refuse)?;
                Ok((path, StudioShapeParameters::Polygon { center_x: l(a[0]), center_y: l(a[1]), radius: l(r), side_count: tool.polygon_sides, inset_percentage: 0.0 }))
            } else {
                let path = shape::star(a[0], a[1], r, tool.star_points, tool.star_inner).map_err(refuse)?;
                Ok((path, StudioShapeParameters::Star { center_x: l(a[0]), center_y: l(a[1]), radius: l(r), point_count: tool.star_points, inner_radius: tool.star_inner }))
            }
        }
        ShapeKind::Line => {
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let mut angle = dy.atan2(dx).to_degrees();
            if constrain {
                angle = (angle / 45.0).round() * 45.0;
            }
            let len = (dx * dx + dy * dy).sqrt();
            let path = shape::line(a[0], a[1], len, angle).map_err(refuse)?;
            Ok((path, StudioShapeParameters::Line { x: l(a[0]), y: l(a[1]), length: l(len), angle }))
        }
        ShapeKind::ArcSegment => {
            let path = shape::arc_segment(a[0], a[1], b[0], b[1], tool.arc_slope, tool.arc_closed, true).map_err(refuse)?;
            Ok((path, StudioShapeParameters::ArcSegment { start_x: l(a[0]), start_y: l(a[1]), end_x: l(b[0]), end_y: l(b[1]), slope: tool.arc_slope, closed: tool.arc_closed, base_along_x: true, fill_arc: false }))
        }
        ShapeKind::RectangularGrid => {
            let path = shape::rectangular_grid(x, y, w, h, tool.grid_rows, tool.grid_columns, 0.0, 0.0, true).map_err(refuse)?;
            Ok((path, StudioShapeParameters::RectangularGrid { x: l(x), y: l(y), width: l(w), height: l(h), horizontal_dividers: tool.grid_rows, vertical_dividers: tool.grid_columns, skew_horizontal: 0.0, skew_vertical: 0.0, use_outside_rectangle_as_frame: true, fill_grid: true }))
        }
        ShapeKind::PolarGrid => {
            let (rx, ry) = (w / 2.0, h / 2.0);
            let path = shape::polar_grid(x + rx, y + ry, rx, ry, tool.polar_concentric, tool.polar_radial, 0.0, 0.0).map_err(refuse)?;
            Ok((path, StudioShapeParameters::PolarGrid { center_x: l(x + rx), center_y: l(y + ry), radius_x: l(rx), radius_y: l(ry), concentric_dividers: tool.polar_concentric, radial_dividers: tool.polar_radial, skew_radial: 0.0, skew_angular: 0.0, create_compound_path_from_ellipses: false, fill_grid: true }))
        }
        ShapeKind::Spiral => {
            let r = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            let path = shape::spiral(a[0], a[1], r, tool.spiral_decay, tool.spiral_turns, true).map_err(refuse)?;
            Ok((path, StudioShapeParameters::Spiral { center_x: l(a[0]), center_y: l(a[1]), radius: l(r), decay: tool.spiral_decay, turns: tool.spiral_turns, clockwise: true }))
        }
    }
}

/// Geometry of a live shape record (the same Nib builders as the shape tools).
fn shape_geometry(p: &StudioShapeParameters) -> Result<AnchorPath, String> {
    let refuse = |e: shape::ShapeError| format!("shape_{}: {}", e.parameter, e.error.code());
    match p {
        StudioShapeParameters::Rectangle { x, y, width, height, corner_radius, corner_kind, corner_smoothing } => {
            let corner_kind = match corner_kind {
                folio::StudioCornerKind::None => CornerKind::None,
                folio::StudioCornerKind::Round => CornerKind::Round,
                folio::StudioCornerKind::RoundInverted => CornerKind::RoundInverted,
                folio::StudioCornerKind::Inset => CornerKind::Inset,
                folio::StudioCornerKind::Bevel => CornerKind::Bevel,
                folio::StudioCornerKind::Fancy => CornerKind::Fancy,
            };
            let spec = RectSpec { x: x.value, y: y.value, width: width.value, height: height.value, corner_radius: corner_radius.clone().map(|r| r.value), corner_kind, corner_smoothing: *corner_smoothing };
            shape::rectangle(&spec).map(|(p, _)| p).map_err(refuse)
        }
        StudioShapeParameters::Ellipse { center_x, center_y, radius_x, radius_y, start_angle, end_angle, inner_ratio } => shape::ellipse(&EllipseSpec {
            cx: center_x.value,
            cy: center_y.value,
            rx: radius_x.value,
            ry: radius_y.value,
            start_angle: *start_angle,
            end_angle: *end_angle,
            inner_ratio: *inner_ratio,
        })
        .map_err(refuse),
        StudioShapeParameters::Polygon { center_x, center_y, radius, side_count, inset_percentage } => shape::polygon(center_x.value, center_y.value, radius.value, *side_count, *inset_percentage).map_err(refuse),
        StudioShapeParameters::Star { center_x, center_y, radius, point_count, inner_radius } => shape::star(center_x.value, center_y.value, radius.value, *point_count, *inner_radius).map_err(refuse),
        StudioShapeParameters::Line { x, y, length, angle } => shape::line(x.value, y.value, length.value, *angle).map_err(refuse),
        StudioShapeParameters::ArcSegment { start_x, start_y, end_x, end_y, slope, closed, base_along_x, .. } => {
            shape::arc_segment(start_x.value, start_y.value, end_x.value, end_y.value, *slope, *closed, *base_along_x).map_err(refuse)
        }
        StudioShapeParameters::RectangularGrid { x, y, width, height, horizontal_dividers, vertical_dividers, skew_horizontal, skew_vertical, use_outside_rectangle_as_frame, .. } => {
            shape::rectangular_grid(x.value, y.value, width.value, height.value, *horizontal_dividers, *vertical_dividers, *skew_horizontal, *skew_vertical, *use_outside_rectangle_as_frame).map_err(refuse)
        }
        StudioShapeParameters::PolarGrid { center_x, center_y, radius_x, radius_y, concentric_dividers, radial_dividers, skew_radial, skew_angular, .. } => {
            shape::polar_grid(center_x.value, center_y.value, radius_x.value, radius_y.value, *concentric_dividers, *radial_dividers, *skew_radial, *skew_angular).map_err(refuse)
        }
        StudioShapeParameters::Spiral { center_x, center_y, radius, decay, turns, clockwise } => shape::spiral(center_x.value, center_y.value, radius.value, *decay, *turns, *clockwise).map_err(refuse),
    }
}

/// A path edit applied to `path`.
fn apply(path: &AnchorPath, edit: PathEdit) -> Result<AnchorPath, String> {
    let r = match edit {
        PathEdit::InsertAnchor { contour, segment, t } => ap::insert_anchor(path, contour, segment, t),
        PathEdit::DeleteAnchor(at) => ap::delete_anchor(path, at),
        PathEdit::Convert(at, m) => ap::convert_anchor(path, at, m),
        PathEdit::MoveAnchor { at, dx, dy, constraint } => ap::move_anchor(path, at, dx, dy, constraint),
        PathEdit::MoveHandle { at, side, to, constraint, break_mirroring } => ap::move_handle(path, at, side, PathPoint::new(to[0], to[1]), constraint, break_mirroring),
        PathEdit::Bend { contour, segment, t, dx, dy } => ap::bend_segment(path, contour, segment, t, dx, dy),
        PathEdit::SplitAt { contour, segment, t } => ap::split_at(path, contour, segment, t),
        PathEdit::SplitAtAnchor(at) => ap::split_at_anchor(path, at),
        PathEdit::Join(a, b) => ap::join(path, a, b, ap::DEFAULT_MERGE_DISTANCE),
        PathEdit::Reverse(c) => ap::reverse(path, c),
        PathEdit::Corner { at, kind, radius } => freeform::apply_corner(path, at, kind, radius).map(|(p, _)| p),
    };
    r.map_err(|e| format!("path_edit: {}", e.code()))
}

/// Tool row hover text of the vector tools.
pub fn tool_hover(tool: Tool) -> &'static str {
    match tool {
        Tool::Pen => "Click for a corner anchor, drag for a smooth anchor; click the first anchor to close; Enter ends the path",
        Tool::DirectSelect => "Drag an anchor, a handle end or a segment of a vector layer (Shift: 45 degrees, Alt on a handle: break mirroring)",
        Tool::Rectangle => "Drag a rectangle (Shift: square)",
        Tool::RoundedRectangle => "Drag a rounded rectangle (corner radius below; Shift: square)",
        Tool::EllipseShape => "Drag an ellipse (Shift: circle)",
        Tool::Polygon => "Drag from the centre: regular polygon (sides below)",
        Tool::Star => "Drag from the centre: star (points and inner radius below)",
        Tool::Line => "Drag a line segment (Shift: 45 degrees)",
        Tool::Curvature => "Click to place points the curve passes through (Alt-click: corner point); click the first point to close",
        Tool::Pencil => "Drag a freehand path, fitted and smoothed (fidelity below); ends close together close the path",
        Tool::Knife => "Drag across filled vector layers to cut them into separate closed regions",
        Tool::ShapeBuilder => "Drag across overlapping regions of vector layers to merge them (Alt: delete the regions)",
        Tool::ArcSegment => "Drag an arc segment (slope below)",
        Tool::RectangularGrid => "Drag a rectangular grid (rows and columns below)",
        Tool::PolarGrid => "Drag a polar grid (concentric and radial dividers below)",
        Tool::Spiral => "Drag from the centre: spiral (decay and turns below)",
        Tool::Width => "Drag from a stroke outward to set its width there (variable-width stroke)",
        Tool::Reshape => "Drag a point of a path; nearby anchors follow smoothly",
        Tool::BlobBrush => "Paint a filled shape that merges with overlapping shapes of the same fill",
        Tool::PaintBucket => "Click an enclosed region of the selected vector layer to fill it (Alt: clear it); the path becomes a vector network",
        _ => "",
    }
}

impl Tool {
    /// The shape a shape tool draws.
    pub fn shape(self) -> Option<ShapeKind> {
        match self {
            Tool::Rectangle => Some(ShapeKind::Rectangle),
            Tool::RoundedRectangle => Some(ShapeKind::RoundedRectangle),
            Tool::EllipseShape => Some(ShapeKind::Ellipse),
            Tool::Polygon => Some(ShapeKind::Polygon),
            Tool::Star => Some(ShapeKind::Star),
            Tool::Line => Some(ShapeKind::Line),
            Tool::ArcSegment => Some(ShapeKind::ArcSegment),
            Tool::RectangularGrid => Some(ShapeKind::RectangularGrid),
            Tool::PolarGrid => Some(ShapeKind::PolarGrid),
            Tool::Spiral => Some(ShapeKind::Spiral),
            _ => None,
        }
    }
    /// Pen, direct selection or a shape tool.
    pub fn is_vector(self) -> bool {
        matches!(
            self,
            Tool::Pen | Tool::DirectSelect | Tool::Curvature | Tool::Pencil | Tool::Knife | Tool::ShapeBuilder | Tool::Width | Tool::Reshape | Tool::BlobBrush | Tool::PaintBucket
        ) || self.shape().is_some()
    }
}

impl Viewer {
    fn vdoc(&self) -> Result<&StudioDocument, String> {
        self.run.snapshot().map(|s| s.document()).ok_or_else(|| "nothing open".to_string())
    }

    /// Vector tool settings (fill, stroke, shape parameters, pen layer).
    pub fn vector_tool(&self) -> &VectorTool {
        &self.edit.vector
    }

    /// Replaces the vector tool settings (the gesture in progress and the pen layer are kept).
    pub fn set_vector_tool(&mut self, tool: VectorTool) {
        let (press, current, pen, vertex) = (self.edit.vector.press, self.edit.vector.current, self.edit.vector.pen_layer.take(), self.edit.vector.pen_vertex);
        self.edit.vector = VectorTool { press, current, pen_layer: pen, pen_vertex: vertex, ..tool };
    }

    /// The entry bytes: the session document entry when the session carries the `vector` token,
    /// else the viewer-local copy (see [`entry_persistable`]).
    fn entry_bytes(&self) -> Option<&[u8]> {
        if entry_persistable() { self.run.document_entry(ENTRY) } else { self.edit.vector_local.as_deref() }
    }

    /// The vector records of the open document (empty when it has none).
    pub fn vector_entry(&self) -> Result<VectorEntry, String> {
        let doc = self.vdoc()?;
        match self.entry_bytes() {
            Some(bytes) => VectorEntry::decode(bytes, doc, self.cancel),
            None => Ok(VectorEntry::empty()),
        }
    }

    /// Save guard: a document with vector layers is saved only when its vector records can be
    /// written with it (no silent loss of path geometry).
    pub(super) fn vector_save_check(&self) -> Result<(), String> {
        let has_vector = self.vdoc().is_ok_and(|d| d.layers.iter().any(|l| matches!(l.payload, Payload::Vector { .. })));
        if has_vector && !entry_persistable() {
            return Err(format!("vector_entry_not_persistable: the session carries no document entry token `{ENTRY}` (session DOCUMENT_ENTRY_TOKENS); vector geometry would be lost"));
        }
        Ok(())
    }

    /// Renders the open document (render-cpu) with its vector layers prepared by composite's Folio
    /// adapter ([`hsk_studio_composite::prepare_vectors`]) instead of the viewer's own specs: path
    /// geometry and `StudioVectorNetwork` records come from the saved vector entry
    /// (`VectorAssets::network`), paint from the caller's Folio `design` records (the viewer
    /// authors none: SPEC GAP), plan blend space linear light like [`super::render::render_full`].
    /// A refused vector layer is an error.
    pub fn render_folio_vectors(&self, design: &folio::StudioDesignRecords, records: &folio::StudioVectorRecords) -> Result<super::render::Frame, String> {
        let snap = self.run.snapshot().ok_or("render_folio_vectors: nothing open")?;
        let entry = self.vector_entry()?;
        let prepared = hsk_studio_composite::prepare_vectors(snap, design, records, &EntryAssets(&entry), hsk_studio_composite::BlendSpace::LinearLight, &VectorResolution::default(), self.cancel)
            .map_err(|e| format!("vector_prepare: {e:?}"))?;
        for l in &snap.document().layers {
            if let Some(code) = prepared.refusal(&l.layer_id) {
                return Err(format!("vector_refused: {} {code}", l.layer_id));
            }
        }
        let text = self.prepared_text()?;
        super::render::render_full(snap, &self.assets, Some(&text), Some(&prepared), super::render::Prefer::Cpu, None, None, self.cancel).map_err(|e| format!("render: {e}"))
    }

    /// Prepared vector layers of the open document (`None` without vector layers): composite's
    /// Folio adapter over the entry's records (module docs); a refused layer is an error.
    pub(super) fn prepared_vectors(&self) -> Result<Option<PreparedVectors>, String> {
        let doc = self.vdoc()?;
        if !doc.layers.iter().any(|l| matches!(l.payload, Payload::Vector { .. })) {
            return Ok(None);
        }
        let snap = self.run.snapshot().ok_or("vector_prepare: nothing open")?;
        let entry = self.vector_entry()?;
        let design = style::effective_design(doc, &entry)?;
        let records = entry.folio_records(&doc.document_id);
        let prepared = hsk_studio_composite::prepare_vectors(snap, &design, &records, &EntryAssets(&entry), hsk_studio_composite::BlendSpace::LinearLight, &VectorResolution::default(), self.cancel)
            .map_err(|e| format!("vector_prepare: {e:?}"))?;
        for l in &doc.layers {
            if let Some(code) = prepared.refusal(&l.layer_id) {
                return Err(format!("vector_refused: {} {code}", l.layer_id));
            }
        }
        Ok(Some(prepared))
    }

    /// The anchor path of a vector layer (its first path) and that path's record.
    pub fn vector_path(&self, layer_id: &str) -> Result<PathRecord, String> {
        let doc = self.vdoc()?;
        let layer = doc.layers.iter().find(|l| l.layer_id == layer_id).ok_or_else(|| format!("no layer {layer_id}"))?;
        let Payload::Vector { path_ids } = &layer.payload else {
            return Err(format!("not_a_vector_layer: {layer_id}"));
        };
        let id = path_ids.first().ok_or("vector_layer_without_path")?;
        self.vector_entry()?.path(id).cloned().ok_or_else(|| format!("vector_path_unresolved: {id}"))
    }

    /// The live shape record of a layer, if its parameters are still editable.
    pub fn live_shape(&self, layer_id: &str) -> Result<Option<StudioShapeParameters>, String> {
        let doc = self.vdoc()?;
        Ok(self.vector_entry()?.applicable_live(doc).shape(layer_id).map(|s| s.shape.clone()))
    }

    fn style_now(&self, pen: bool) -> VectorStyle {
        let t = &self.edit.vector;
        if pen {
            VectorStyle { fill: None, stroke: Some(t.stroke), stroke_width: t.stroke_width }
        } else {
            VectorStyle { fill: t.fill_on.then_some(t.fill), stroke: t.stroke_on.then_some(t.stroke), stroke_width: t.stroke_width }
        }
    }

    /// Stores `entry` (validated against the document) before the transaction that references it.
    fn store_entry(&mut self, entry: &VectorEntry) -> Result<(), String> {
        let doc = self.vdoc()?;
        let bytes = entry.to_bytes(&doc.document_id)?;
        if entry_persistable() {
            self.run.set_document_entry(ENTRY, Some(bytes)).map_err(|e| format!("vector_entry: {e}"))
        } else {
            self.edit.vector_local = Some(bytes);
            Ok(())
        }
    }

    /// Creates a vector layer holding `path` above the selected layer (one transaction); selects it.
    fn create_vector_layer(&mut self, op: &str, name: &str, path: AnchorPath, style: VectorStyle, live: Option<StudioShapeParameters>) -> Result<String, String> {
        let (parent, place) = self.insertion_point()?;
        let mut entry = self.vector_entry()?;
        let doc = self.vdoc()?;
        let id = super::layers::mint_layer_id(doc, u64::try_from(doc.layers.len()).unwrap_or(0));
        let path_id = mint_path_id(doc, &entry);
        path.validate().map_err(|e| format!("{op}: {}", e.code()))?;
        if entry.paths.len() >= MAX_PATHS {
            return Err(format!("{op}: vector_entry_over_budget"));
        }
        entry.paths.push(PathRecord::plain(path_id.clone(), path, style));
        if let Some(shape) = live {
            entry.live.push(StudioLiveShape { layer_id: id.clone(), path_id: path_id.clone(), shape });
        }
        let layer = super::layers::new_layer(&id, name, &parent, "vector", json!({"payload_kind": "vector", "path_ids": [path_id]}))?;
        let operation = super::layers::operation_for(&id, false)?;
        self.store_entry(&entry)?;
        self.structure(op, vec![PlacementWrite { layer_id: id.clone(), to: Some(place), create: Some((layer, operation)) }], format!("{op} {id}"))?;
        self.selected = Some(id.clone());
        Ok(id)
    }

    /// Replaces the geometry of vector layer `layer` with `path` (and `live`, when the shape stays
    /// live): the new path record is stored, then ONE chronicle `vector_paths` write commits the new
    /// path id (lane SMB-ILLUSTRATOR-PEN-2): the layer keeps its id, name, place and appearance and
    /// the edit undoes as one step. Selects the layer and returns its id.
    fn replace_vector_layer(&mut self, op: &str, layer: &str, path: AnchorPath, style: Option<VectorStyle>, live: Option<StudioShapeParameters>) -> Result<String, String> {
        path.validate().map_err(|e| format!("{op}: {}", e.code()))?;
        let record = self.vector_path(layer)?;
        let mut entry = self.vector_entry()?;
        if entry.paths.len() >= MAX_PATHS {
            return Err(format!("{op}: vector_entry_over_budget"));
        }
        let path_id = mint_path_id(self.vdoc()?, &entry);
        entry.paths.push(PathRecord { width: record.width.clone(), ..PathRecord::plain(path_id.clone(), path, style.unwrap_or(record.style)) });
        if let Some(shape) = live {
            entry.live.push(StudioLiveShape { layer_id: layer.to_string(), path_id: path_id.clone(), shape });
        }
        self.store_entry(&entry)?;
        let ok = self.run.set_vector_paths(op, layer, vec![path_id]);
        self.after_edit(ok, format!("{op} {layer}"))?;
        self.selected = Some(layer.to_string());
        Ok(layer.to_string())
    }

    fn report<T>(&mut self, result: Result<T, String>) -> Result<T, String> {
        if let Err(e) = &result {
            self.say(format!("vector: {e}"));
        }
        result
    }

    // ----- pen ----------------------------------------------------------------------------------

    /// Pen click (`drag` = None: corner anchor) or drag (`drag` = the out-handle vector: smooth
    /// anchor with mirrored handles) at document pixel (`x`, `y`). Without an active pen path it
    /// starts a new vector layer; otherwise it extends the pen layer's open contour, or closes it
    /// when the point is within `close_radius` of its first anchor (which ends the pen path). One
    /// transaction per call. Returns the pen layer id.
    pub fn pen_point(&mut self, x: f64, y: f64, drag: Option<[f64; 2]>, close_radius: f64) -> Result<String, String> {
        let r = self.pen_point_inner(x, y, drag, close_radius);
        self.report(r)
    }

    fn pen_point_inner(&mut self, x: f64, y: f64, drag: Option<[f64; 2]>, close_radius: f64) -> Result<String, String> {
        if !(x.is_finite() && y.is_finite() && drag.is_none_or(|d| d.iter().all(|v| v.is_finite())) && close_radius.is_finite()) {
            return Err("pen: non_finite".into());
        }
        // A network layer (the pen layer, else the selected layer) is extended as a network: the
        // click joins an existing vertex or edge (lane SMB-COMPOSITE-STROKE-GEOMETRY slice 3).
        let network_layer = self.edit.vector.pen_layer.clone().or_else(|| self.selected.clone()).filter(|id| self.vector_path(id).is_ok_and(|r| r.network.is_some()));
        if let Some(layer) = network_layer {
            return self.network_pen_inner(&layer, x, y, drag, close_radius);
        }
        let anchor = match drag {
            Some([dx, dy]) if dx != 0.0 || dy != 0.0 => EditAnchor::smooth(x, y, dx, dy),
            _ => EditAnchor::corner(x, y),
        };
        // The pen layer, while it exists with an open last contour.
        let current = self.edit.vector.pen_layer.clone().and_then(|id| self.vector_path(&id).ok().map(|r| (id, r)));
        match current {
            Some((id, record)) if record.path.contours.last().is_some_and(|c| !c.closed) => {
                let last = record.path.contours.len().saturating_sub(1);
                let contour = &record.path.contours[last];
                let first = contour.anchors[0].position;
                let near = ((first.x - x).powi(2) + (first.y - y).powi(2)).sqrt() <= close_radius;
                let path = if near && contour.anchors.len() >= 2 {
                    let incoming = drag.map(|[dx, dy]| PathPoint::new(-dx, -dy));
                    ap::close_contour(&record.path, last, incoming).map_err(|e| format!("pen: {}", e.code()))?
                } else {
                    ap::append_anchor(&record.path, last, anchor).map_err(|e| format!("pen: {}", e.code()))?
                };
                let closed = path.contours.last().is_some_and(|c| c.closed);
                let new = self.replace_vector_layer("pen", &id, path, None, None)?;
                if closed {
                    self.edit.vector.pen_layer = None;
                }
                Ok(new)
            }
            _ => {
                let path = ap::start_contour(&AnchorPath::new(FillRule::NonZero), anchor).map_err(|e| format!("pen: {}", e.code()))?;
                let style = self.style_now(true);
                let id = self.create_vector_layer("pen", "Path", path, style, None)?;
                self.edit.vector.pen_layer = Some(id.clone());
                Ok(id)
            }
        }
    }

    /// Curvature tool (STU-VEC-007): adds a point the curve passes through (`corner` = Alt: a
    /// corner point) to the curvature path, or closes it when the point is within `close_radius` of
    /// its first point; every smooth point's handles are recomputed ([`freeform::smooth_curvature`]).
    /// One transaction per call (a new layer for the first point, a `vector_paths` write after).
    pub fn curvature_point(&mut self, x: f64, y: f64, corner: bool, close_radius: f64) -> Result<String, String> {
        let r = self.curvature_inner(x, y, corner, close_radius);
        self.report(r)
    }

    fn curvature_inner(&mut self, x: f64, y: f64, corner: bool, close_radius: f64) -> Result<String, String> {
        if !(x.is_finite() && y.is_finite() && close_radius.is_finite()) {
            return Err("curvature: non_finite".into());
        }
        let anchor = EditAnchor { mirroring: if corner { Mirroring::None } else { Mirroring::AngleAndLength }, ..EditAnchor::corner(x, y) };
        let current = self.edit.vector.pen_layer.clone().and_then(|id| self.vector_path(&id).ok().map(|r| (id, r)));
        match current {
            Some((id, record)) if record.path.contours.last().is_some_and(|c| !c.closed) => {
                let last = record.path.contours.len().saturating_sub(1);
                let contour = &record.path.contours[last];
                let first = contour.anchors[0].position;
                let near = ((first.x - x).powi(2) + (first.y - y).powi(2)).sqrt() <= close_radius;
                let grown = if near && contour.anchors.len() >= 2 {
                    ap::close_contour(&record.path, last, None)
                } else {
                    ap::append_anchor(&record.path, last, anchor)
                }
                .map_err(|e| format!("curvature: {}", e.code()))?;
                let closed = grown.contours.last().is_some_and(|c| c.closed);
                let path = freeform::smooth_curvature(&grown, last).map_err(|e| format!("curvature: {}", e.code()))?;
                let id = self.replace_vector_layer("curvature", &id, path, None, None)?;
                if closed {
                    self.edit.vector.pen_layer = None;
                }
                Ok(id)
            }
            _ => {
                let path = ap::start_contour(&AnchorPath::new(FillRule::NonZero), anchor).map_err(|e| format!("curvature: {}", e.code()))?;
                let style = self.style_now(true);
                let id = self.create_vector_layer("curvature", "Curve", path, style, None)?;
                self.edit.vector.pen_layer = Some(id.clone());
                Ok(id)
            }
        }
    }

    /// Pencil (STU-VEC-007 freehand, STU-VEC-119 fidelity): one new vector layer from the samples,
    /// reduced at `0.5 * 2^fidelity` px and smoothed; closed when the ends are within
    /// `pencil_close_within` px. One transaction.
    pub fn pencil_stroke(&mut self, samples: &[[f64; 2]]) -> Result<String, String> {
        let r = (|| {
            let t = &self.edit.vector;
            if t.pencil_fidelity > 4 {
                return Err("pencil: fidelity 0..=4".to_string());
            }
            let tolerance = [0.5, 1.0, 2.0, 4.0, 8.0][usize::try_from(t.pencil_fidelity).unwrap_or(0)];
            let close = (t.pencil_close_within > 0.0).then_some(t.pencil_close_within);
            let points: Vec<PathPoint> = samples.iter().map(|s| PathPoint::new(s[0], s[1])).collect();
            let path = freeform::fit_freehand(&points, tolerance, close).map_err(|e| format!("pencil: {}", e.code()))?;
            let style = self.style_now(true);
            self.create_vector_layer("pencil", "Pencil", path, style, None)
        })();
        self.report(r)
    }

    /// The vector layers of the selected layer's stack (the document root without a selection),
    /// bottom first.
    pub fn stack_vector_layers(&self) -> Vec<String> {
        let Ok(doc) = self.vdoc() else { return Vec::new() };
        let group = self.selected.as_deref().and_then(|s| self.run.placement(s)).and_then(|(p, _)| match p {
            Parent::Root(()) => None,
            Parent::Node(n) => Some(n.node_id),
        });
        super::layers::stack(doc, group.as_deref())
            .into_iter()
            .filter(|id| doc.layers.iter().any(|l| &l.layer_id == id && matches!(l.payload, Payload::Vector { .. })))
            .collect()
    }

    /// Knife (STU-VEC-048): cuts every filled vector layer of the current stack that the polyline
    /// separates into closed regions; each cut layer is replaced by its regions (same style and
    /// name) in ONE transaction. Typed: `knife: nothing cut` when no layer separates.
    pub fn knife_cut(&mut self, cut: &[[f64; 2]]) -> Result<Vec<String>, String> {
        let r = self.knife_inner(cut);
        self.report(r)
    }

    fn knife_inner(&mut self, cut: &[[f64; 2]]) -> Result<Vec<String>, String> {
        let points: Vec<PathPoint> = cut.iter().map(|s| PathPoint::new(s[0], s[1])).collect();
        let kerf = self.edit.vector.knife_kerf;
        let mut cuts: Vec<(String, PathRecord, Vec<AnchorPath>)> = Vec::new();
        for id in self.stack_vector_layers() {
            let record = self.vector_path(&id)?;
            if record.network.is_some() {
                continue;
            }
            if record.style.fill.is_none() {
                // Strokes only: open pieces (STU-VEC-048), lane SMB-ILLUSTRATOR-PEN-3.
                let pieces = freeform::knife_strokes(&record.path, &points).map_err(|e| format!("knife: {}", e.code()))?;
                if pieces.len() > record.path.contours.len() {
                    cuts.push((id, record, pieces));
                }
                continue;
            }
            let owned = record.path.to_owned_path().map_err(|e| format!("knife: {}", e.code()))?;
            match freeform::knife(&owned, &points, kerf, &BooleanOptions::default()) {
                Ok(pieces) if pieces.len() > 1 => {
                    let pieces = pieces.iter().map(AnchorPath::from_owned_path).collect::<Result<Vec<_>, _>>().map_err(|e| format!("knife: {}", e.code()))?;
                    cuts.push((id, record, pieces));
                }
                Ok(_) | Err(hsk_studio_nib::Error::EmptyPathfinderResult) => {}
                Err(e) => return Err(format!("knife: {e:?}")),
            }
        }
        if cuts.is_empty() {
            return Err("knife: nothing cut".into());
        }
        let pieces: Vec<(String, AnchorPath, VectorStyle)> = cuts.into_iter().flat_map(|(id, rec, pieces)| pieces.into_iter().map(move |p| (id.clone(), p, rec.style))).collect();
        self.replace_with_pieces("knife", pieces)
    }

    /// Shape builder (STU-VEC-007): the given vector layers (one stack) are divided into faces; the
    /// faces the gesture touches are merged (`delete` = false) or removed; every remaining face is a
    /// vector layer; the operands are deleted; ONE transaction.
    pub fn shape_builder_layers(&mut self, layer_ids: &[String], gesture: &[[f64; 2]], delete: bool) -> Result<Vec<String>, String> {
        let r = (|| {
            let mut operands = Vec::new();
            for id in layer_ids {
                let (_, index) = self.run.placement(id).ok_or_else(|| format!("shape_builder: {id} is not in a layer stack"))?;
                operands.push((index, id.clone(), self.vector_path(id)?));
            }
            operands.sort_by_key(|(i, _, _)| *i);
            let paths = operands.iter().map(|(_, _, r)| r.path.to_owned_path().map_err(|e| format!("shape_builder: {}", e.code()))).collect::<Result<Vec<_>, _>>()?;
            let points: Vec<PathPoint> = gesture.iter().map(|s| PathPoint::new(s[0], s[1])).collect();
            let faces = freeform::shape_builder(&paths, &points, delete, &BooleanOptions::default()).map_err(|e| format!("shape_builder: {e:?}"))?;
            let top = operands.last().map(|(_, id, _)| id.clone()).ok_or("shape_builder: no operands")?;
            let mut pieces = Vec::new();
            for f in faces {
                let style = f.sources.iter().max().and_then(|i| operands.get(*i)).map(|(_, _, r)| r.style).ok_or("shape_builder: piece without source")?;
                pieces.push((top.clone(), AnchorPath::from_owned_path(&f.path).map_err(|e| format!("shape_builder: {}", e.code()))?, style));
            }
            let doomed: Vec<String> = operands.iter().map(|(_, id, _)| id.clone()).collect();
            self.replace_with_pieces_deleting("shape_builder", pieces, &doomed)
        })();
        self.report(r)
    }

    /// One transaction: every piece becomes a vector layer directly above its anchor layer (same
    /// parent), then every anchor layer is deleted.
    fn replace_with_pieces(&mut self, op: &str, pieces: Vec<(String, AnchorPath, VectorStyle)>) -> Result<Vec<String>, String> {
        let mut doomed: Vec<String> = Vec::new();
        for (id, _, _) in &pieces {
            if !doomed.contains(id) {
                doomed.push(id.clone());
            }
        }
        self.replace_with_pieces_deleting(op, pieces, &doomed)
    }

    fn replace_with_pieces_deleting(&mut self, op: &str, pieces: Vec<(String, AnchorPath, VectorStyle)>, doomed: &[String]) -> Result<Vec<String>, String> {
        let mut entry = self.vector_entry()?;
        if entry.paths.len().saturating_add(pieces.len()) > MAX_PATHS {
            return Err(format!("{op}: vector_entry_over_budget"));
        }
        let doc = self.vdoc()?;
        let mut writes = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        let mut salt = u64::try_from(doc.layers.len()).unwrap_or(0);
        for (anchor, path, style) in pieces {
            let base = doc.layers.iter().find(|l| l.layer_id == anchor).ok_or_else(|| format!("{op}: no layer {anchor}"))?;
            let mut id = super::layers::mint_layer_id(doc, salt);
            while ids.contains(&id) {
                salt = salt.wrapping_add(1);
                id = super::layers::mint_layer_id(doc, salt);
            }
            salt = salt.wrapping_add(1);
            let path_id = mint_path_id(doc, &entry);
            entry.paths.push(PathRecord::plain(path_id.clone(), path, style));
            let layer = super::layers::new_layer(&id, &base.name, &base.parent, "vector", json!({"payload_kind": "vector", "path_ids": [path_id]}))?;
            writes.push(PlacementWrite { layer_id: id.clone(), to: Some(Place::Before(anchor.clone())), create: Some((layer, super::layers::operation_for(&id, false)?)) });
            ids.push(id);
        }
        writes.extend(doomed.iter().map(|id| PlacementWrite { layer_id: id.clone(), to: None, create: None }));
        self.store_entry(&entry)?;
        self.structure(op, writes, format!("{op} -> {} layer(s)", ids.len()))?;
        self.selected = ids.first().cloned();
        Ok(ids)
    }

    // ----- lane SMB-ILLUSTRATOR-PEN-3: networks, width, reshape, blob brush ------------------------

    /// Stores `record` (a fresh path id is minted) and commits it with ONE `vector_paths` write; the
    /// layer keeps its id.
    fn replace_record(&mut self, op: &str, layer: &str, mut record: PathRecord) -> Result<String, String> {
        record.path.validate().map_err(|e| format!("{op}: {}", e.code()))?;
        let mut entry = self.vector_entry()?;
        if entry.paths.len() >= MAX_PATHS {
            return Err(format!("{op}: vector_entry_over_budget"));
        }
        let path_id = mint_path_id(self.vdoc()?, &entry);
        record.path_id.clone_from(&path_id);
        if let Some(n) = &mut record.network {
            n.path_id.clone_from(&path_id);
            folio::validate_vector_network(n).map_err(|d| format!("{op}: network {:?} {}", d.code, d.target))?;
        }
        entry.paths.push(record);
        self.store_entry(&entry)?;
        let ok = self.run.set_vector_paths(op, layer, vec![path_id]);
        self.after_edit(ok, format!("{op} {layer}"))?;
        self.selected = Some(layer.to_string());
        Ok(layer.to_string())
    }

    /// The network of a vector layer: its stored network, else its path promoted losslessly
    /// (STU-VEC-001; nothing is committed).
    pub fn vector_network(&self, layer_id: &str) -> Result<VectorNetwork, String> {
        let record = self.vector_path(layer_id)?;
        match &record.network {
            Some(n) => network_of(n),
            None => VectorNetwork::from_path(&record.path).map_err(|e| format!("network: {}", e.code())),
        }
    }

    /// Replaces the layer's geometry by `network` (STU-VEC-108 atomic set-network): validated whole,
    /// stored as the Folio `StudioVectorNetwork` record of a new path (whose own geometry is the
    /// network's stroke geometry, for consumers that ignore networks); ONE transaction, same layer.
    pub fn set_network(&mut self, layer_id: &str, network: &VectorNetwork) -> Result<String, String> {
        let r = self.set_network_inner("set_network", layer_id, network);
        self.report(r)
    }

    fn set_network_inner(&mut self, op: &str, layer_id: &str, network: &VectorNetwork) -> Result<String, String> {
        let style = self.vector_path(layer_id)?.style;
        self.set_network_styled(op, layer_id, network, style)
    }

    fn set_network_styled(&mut self, op: &str, layer_id: &str, network: &VectorNetwork, style: VectorStyle) -> Result<String, String> {
        network.validate().map_err(|e| format!("{op}: {}", e.code()))?;
        let record = self.vector_path(layer_id)?;
        let path = network.stroke_geometry().map_err(|e| format!("{op}: {}", e.code()))?;
        let folio_network = folio_of(network, &record.path_id);
        let next = PathRecord { path_id: record.path_id.clone(), path, style, network: Some(folio_network), width: record.width.clone() };
        self.replace_record(op, layer_id, next)
    }

    /// Promotes a vector layer's path to a network (STU-VEC-001, lossless): one transaction.
    pub fn promote_network(&mut self, layer_id: &str) -> Result<String, String> {
        let r = (|| {
            let net = VectorNetwork::from_path(&self.vector_path(layer_id)?.path).map_err(|e| format!("promote: {}", e.code()))?;
            self.set_network_inner("promote_network", layer_id, &net)
        })();
        self.report(r)
    }

    /// Network edit of the model surface (one transaction each): move a vertex, add a vertex,
    /// connect two vertices, delete a segment (regions using it are dropped), set tangents.
    pub fn edit_network(&mut self, layer_id: &str, edit: NetworkEdit) -> Result<String, String> {
        let r = (|| {
            let mut net = self.vector_network(layer_id)?;
            match edit {
                NetworkEdit::MoveVertex { vertex, dx, dy } => {
                    if !(dx.is_finite() && dy.is_finite()) {
                        return Err("network: non_finite".into());
                    }
                    let v = net.vertices.get_mut(vertex).ok_or("network: index_out_of_bounds")?;
                    v.x += dx;
                    v.y += dy;
                }
                NetworkEdit::AddVertex { x, y } => net.vertices.push(NetworkVertex::at(x, y)),
                NetworkEdit::Connect { a, b } => net.segments.push(NetworkSegment::line(a, b)),
                NetworkEdit::DeleteSegment(s) => {
                    if s >= net.segments.len() {
                        return Err("network: index_out_of_bounds".into());
                    }
                    net.segments.remove(s);
                    net.regions.retain(|r| r.loops.iter().all(|l| !l.contains(&s)));
                    for r in &mut net.regions {
                        for l in &mut r.loops {
                            for k in l.iter_mut() {
                                if *k > s {
                                    *k = k.saturating_sub(1);
                                }
                            }
                        }
                    }
                }
                NetworkEdit::Tangents { segment, start, end } => {
                    let seg = net.segments.get_mut(segment).ok_or("network: index_out_of_bounds")?;
                    seg.tangent_start = start.map(|t| PathPoint::new(t[0], t[1]));
                    seg.tangent_end = end.map(|t| PathPoint::new(t[0], t[1]));
                }
            }
            self.set_network_inner("edit_network", layer_id, &net)
        })();
        self.report(r)
    }

    /// Paint bucket (STU-VEC-004, Live-Paint reading): fills (`fill`) or clears the enclosed face of
    /// the layer's network under document point `p`. The network is planarized (crossings and
    /// T-junctions split); its regions become the faces of the planar embedding, each filled when an
    /// interior point of it was inside the previously filled geometry; the face under the point takes
    /// `fill`. A fill on a layer without fill colour takes the tool's fill colour. ONE transaction;
    /// typed `paint_bucket: no enclosed region` when the point is in no face. Studio reading,
    /// Illustrator Live Paint parity UNVERIFIED (D5).
    pub fn network_paint_bucket(&mut self, layer_id: &str, p: [f64; 2], fill: bool) -> Result<String, String> {
        let r = self.paint_bucket_inner("paint_bucket", layer_id, p, fill, None);
        self.report(r)
    }

    /// Paint bucket with a swatch (lane SMB-VECTOR-STYLES): the face at `p` is filled and names
    /// swatch `swatch_id` as its region fill style (`None`: back to the layer's fill), so composite
    /// paints it with the swatch and a swatch edit repaints it. ONE transaction.
    pub fn network_region_swatch(&mut self, layer_id: &str, p: [f64; 2], swatch_id: Option<&str>) -> Result<String, String> {
        let r = (|| {
            if let Some(id) = swatch_id {
                let entry = self.vector_entry()?;
                let known = entry.design.as_ref().is_some_and(|d| d.paint_styles.iter().any(|s| s.style_id == id && style::is_swatch(s)));
                if !known {
                    return Err(format!("region_swatch: unknown swatch {id}"));
                }
            }
            self.paint_bucket_inner("region_swatch", layer_id, p, true, Some(swatch_id.map(str::to_string)))
        })();
        self.report(r)
    }

    /// Re-detects the faces of the planarized network, carrying each old filled region's fill state
    /// and region style to the faces inside it, then sets the face at `p` (`style`: `Some` sets its
    /// region style).
    fn paint_bucket_inner(&mut self, op: &str, layer_id: &str, p: [f64; 2], fill: bool, style: Option<Option<String>>) -> Result<String, String> {
        {
            if !p.iter().all(|v| v.is_finite()) {
                return Err(format!("{op}: non_finite"));
            }
            let code = |e: hsk_studio_nib::network::NetworkError| format!("{op}: {}", e.code());
            let network = self.vector_network(layer_id)?;
            let planar = network.planarize(PLANAR_TOLERANCE).map_err(code)?;
            // Fill state only shows where the layer has a fill: without one nothing was filled.
            let record = self.vector_path(layer_id)?;
            let painted = style::has_fill(&self.vector_entry()?, layer_id, &record);
            let mut old: Vec<(AnchorPath, Option<String>)> = Vec::new();
            for r in planar.regions.iter().filter(|r| r.fills) {
                let probe = VectorNetwork { regions: vec![r.clone()], ..planar.clone() };
                if r.fill_style_id.is_none() && !painted {
                    continue;
                }
                old.extend(probe.fill_geometry().map_err(code)?.into_iter().map(|g| (g, r.fill_style_id.clone())));
            }
            let q = PathPoint::new(p[0], p[1]);
            let mut faces = planar.detect_regions(false).map_err(code)?;
            let mut found = false;
            for face in &mut faces {
                let probe = VectorNetwork { regions: vec![NetworkRegion { fills: true, ..face.clone() }], ..planar.clone() };
                let geometry = probe.fill_geometry().map_err(code)?;
                let Some(shape) = geometry.first() else { continue };
                if let Some(inner) = interior_point(shape) {
                    let carried = old.iter().find(|(g, _)| region_contains(g, inner));
                    face.fills = carried.is_some();
                    face.fill_style_id = carried.and_then(|(_, s)| s.clone());
                }
                if !found && region_contains(shape, q) {
                    face.fills = fill;
                    if let Some(s) = &style {
                        face.fill_style_id.clone_from(s);
                    }
                    if !fill {
                        face.fill_style_id = None;
                    }
                    found = true;
                }
            }
            if !found {
                return Err(format!("{op}: no enclosed region at the point"));
            }
            let next = VectorNetwork { regions: faces, ..planar };
            let mut paint = record.style;
            if fill && !painted && style.is_none() {
                paint.fill = Some(self.edit.vector.fill);
            }
            self.set_network_styled(op, layer_id, &next, paint)
        }
    }

    /// Width tool (STU-VEC-007, STU-VEC-122): sets the stroke width at arc-length fraction `t`
    /// (0..=1 of each contour) of the layer's stroke (adding or replacing a width point; the first
    /// point starts from the uniform stroke width). ONE transaction.
    pub fn set_width_point(&mut self, layer_id: &str, t: f64, width: f64) -> Result<String, String> {
        let r = (|| {
            let record = self.vector_path(layer_id)?;
            let base = record.width.as_ref().map(|w| width_profile(w)).transpose()?.unwrap_or_else(|| WidthProfile::uniform(record.style.stroke_width));
            let profile = base.with_point(t, width).map_err(|e| format!("width: {e:?}"))?;
            let next = PathRecord { width: Some(profile.points.iter().map(|p| [p.t, p.width]).collect()), ..record };
            self.replace_record("width", layer_id, next)
        })();
        self.report(r)
    }

    /// Removes the layer's width profile (back to the uniform stroke). ONE transaction.
    pub fn clear_width(&mut self, layer_id: &str) -> Result<String, String> {
        let r = (|| {
            let record = self.vector_path(layer_id)?;
            let next = PathRecord { width: None, ..record };
            self.replace_record("clear_width", layer_id, next)
        })();
        self.report(r)
    }

    /// Width tool gesture: the stroke point nearest `from` gets the width `2 * |to - point|`.
    pub fn width_drag(&mut self, from: [f64; 2], to: [f64; 2], radius: f64) -> Result<String, String> {
        let (layer, target) = self.direct_hit(from, radius).ok_or("width: nothing under the pointer")?;
        let record = self.vector_path(&layer)?;
        let (contour, segment, t) = match target {
            DirectTarget::Segment { contour, segment, t } => (contour, segment, t),
            DirectTarget::Anchor(at) | DirectTarget::Handle(at, _) => (at.contour, at.anchor.min(record.path.contour(at.contour).map_err(|e| e.code().to_string())?.segment_count().saturating_sub(1)), 0.0),
        };
        let on = ap::point_at(&record.path, contour, segment, t).map_err(|e| e.code().to_string())?;
        let width = 2.0 * ((to[0] - on.x).powi(2) + (to[1] - on.y).powi(2)).sqrt();
        let fraction = arc_fraction(&record.path, contour, segment, t)?;
        self.set_width_point(&layer, fraction, width)
    }

    /// Reshape (STU-VEC-007): drags the path point under `from` to `to`; anchors within
    /// `reshape_radius` along the path follow with a falloff, continuity kept. ONE transaction.
    pub fn reshape_drag(&mut self, from: [f64; 2], to: [f64; 2], radius: f64) -> Result<String, String> {
        let r = (|| {
            let (layer, target) = self.direct_hit(from, radius).ok_or("reshape: nothing under the pointer")?;
            let DirectTarget::Segment { contour, segment, t } = target else {
                return self.direct_drag_inner(from, to, Constraint::Free, false, radius);
            };
            let record = self.vector_path(&layer)?;
            let path = freeform::reshape(&record.path, contour, segment, t, to[0] - from[0], to[1] - from[1], self.edit.vector.reshape_radius).map_err(|e| format!("reshape: {}", e.code()))?;
            self.replace_vector_layer("reshape", &layer, path, None, None)
        })();
        self.report(r)
    }

    /// Blob brush (STU-VEC-007): the brush outline of the samples (`blob_diameter`, round), united
    /// with every vector layer of the current stack that has the same paint (fill colour, no stroke,
    /// no network) and overlaps it: ONE transaction creating the united layer and deleting the merged
    /// ones.
    pub fn blob_brush_stroke(&mut self, samples: &[[f64; 2]]) -> Result<String, String> {
        let r = (|| {
            let t = &self.edit.vector;
            let style = VectorStyle { fill: Some(t.fill), stroke: None, stroke_width: 0.0 };
            let diameter = t.blob_diameter;
            let mut candidates = Vec::new();
            for id in self.stack_vector_layers() {
                let record = self.vector_path(&id)?;
                if record.style == style && record.network.is_none() {
                    candidates.push((id, record.path.to_owned_path().map_err(|e| format!("blob: {}", e.code()))?));
                }
            }
            let points: Vec<PathPoint> = samples.iter().map(|s| PathPoint::new(s[0], s[1])).collect();
            let others: Vec<OwnedPath> = candidates.iter().map(|(_, p)| p.clone()).collect();
            let (united, merged) = freeform::blob_brush(&points, diameter, &others, &BooleanOptions::default()).map_err(|e| format!("blob: {e:?}"))?;
            let path = AnchorPath::from_owned_path(&united).map_err(|e| format!("blob: {}", e.code()))?;
            let merged_ids: Vec<String> = merged.iter().filter_map(|i| candidates.get(*i).map(|(id, _)| id.clone())).collect();
            match merged_ids.last() {
                None => self.create_vector_layer("blob_brush", "Blob", path, style, None),
                Some(top) => {
                    let ids = self.replace_with_pieces_deleting("blob_brush", vec![(top.clone(), path, style)], &merged_ids)?;
                    ids.into_iter().next().ok_or_else(|| "blob: no layer".to_string())
                }
            }
        })();
        self.report(r)
    }

    /// Ends the pen path (the next pen click starts a new one). No transaction.
    pub fn pen_end(&mut self) {
        self.edit.vector.pen_layer = None;
        self.edit.vector.pen_vertex = None;
        self.edit.vector.press = None;
    }

    /// Pen on a vector network (STU-VEC-007 "connect to any existing network anchor, not only
    /// endpoints"; STU-VEC-008 the pen contributes to the network instead of collapsing it): the
    /// click lands on the vertex within `radius`, else splits the edge under it, else adds a vertex,
    /// and is joined to the previous pen vertex (`drag` = its tangent). ONE transaction; returns the
    /// layer id; the pen keeps the vertex for the next click until [`Viewer::pen_end`].
    pub fn network_pen(&mut self, layer_id: &str, x: f64, y: f64, drag: Option<[f64; 2]>, radius: f64) -> Result<String, String> {
        let r = self.network_pen_inner(layer_id, x, y, drag, radius);
        self.report(r)
    }

    fn network_pen_inner(&mut self, layer_id: &str, x: f64, y: f64, drag: Option<[f64; 2]>, radius: f64) -> Result<String, String> {
        let net = self.vector_network(layer_id)?;
        let from = if self.edit.vector.pen_layer.as_deref() == Some(layer_id) { self.edit.vector.pen_vertex } else { None };
        let (next, target) = net.pen(from, PathPoint::new(x, y), drag.map(|d| PathPoint::new(d[0], d[1])), radius).map_err(|e| format!("pen: {}", e.code()))?;
        let id = self.set_network_inner("pen_network", layer_id, &next)?;
        self.edit.vector.pen_layer = Some(layer_id.to_string());
        self.edit.vector.pen_vertex = Some(target);
        Ok(id)
    }

    // ----- direct selection -----------------------------------------------------------------------

    /// The vector layer and part under document point `p` within `radius`: the selected vector layer
    /// first, then every vector layer top-first; handles and anchors before segments.
    pub fn direct_hit(&self, p: [f64; 2], radius: f64) -> Option<(String, DirectTarget)> {
        let mut order: Vec<String> = self.selected.iter().cloned().collect();
        order.extend(self.layers().into_iter().filter(|r| r.kind == "vector").map(|r| r.layer_id));
        let q = PathPoint::new(p[0], p[1]);
        for id in order {
            let Ok(record) = self.vector_path(&id) else { continue };
            if let Some(hit) = ap::hit_anchor(&record.path, q, radius) {
                let target = match hit.part {
                    AnchorSelection::InHandle => DirectTarget::Handle(hit.at, HandleSide::In),
                    AnchorSelection::OutHandle => DirectTarget::Handle(hit.at, HandleSide::Out),
                    _ => DirectTarget::Anchor(hit.at),
                };
                return Some((id, target));
            }
            if let Some(s) = ap::hit_segment(&record.path, q, radius) {
                return Some((id, DirectTarget::Segment { contour: s.contour, segment: s.segment, t: s.t }));
            }
        }
        None
    }

    /// Direct-selection drag from `from` to `to` (document pixels): moves the anchor (with its
    /// handles), sets the handle end (mirroring per the anchor; `break_mirroring` = Alt) or bends the
    /// segment so the grabbed point follows (STU-VEC-003). Shift = `Constraint::Angle45`. One
    /// transaction; a live shape is expanded. Returns the layer id (unchanged).
    pub fn direct_drag(&mut self, from: [f64; 2], to: [f64; 2], constraint: Constraint, break_mirroring: bool, radius: f64) -> Result<String, String> {
        let r = self.direct_drag_inner(from, to, constraint, break_mirroring, radius);
        self.report(r)
    }

    fn direct_drag_inner(&mut self, from: [f64; 2], to: [f64; 2], constraint: Constraint, break_mirroring: bool, radius: f64) -> Result<String, String> {
        if !from.iter().chain(&to).all(|v| v.is_finite()) || !radius.is_finite() {
            return Err("direct_select: non_finite".into());
        }
        // Network layers: drag the nearest vertex (lane SMB-ILLUSTRATOR-PEN-3).
        if let Some(layer) = self.selected.clone()
            && self.vector_path(&layer).is_ok_and(|r| r.network.is_some())
        {
            let net = self.vector_network(&layer)?;
            let near = net.vertices.iter().enumerate().map(|(i, v)| (i, ((v.x - from[0]).powi(2) + (v.y - from[1]).powi(2)).sqrt())).filter(|(_, d)| *d <= radius).min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((vertex, _)) = near {
                return self.edit_network(&layer, NetworkEdit::MoveVertex { vertex, dx: to[0] - from[0], dy: to[1] - from[1] });
            }
        }
        let (layer, target) = self.direct_hit(from, radius).ok_or("direct_select: nothing under the pointer")?;
        let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
        let edit = match target {
            DirectTarget::Anchor(at) => PathEdit::MoveAnchor { at, dx, dy, constraint },
            DirectTarget::Handle(at, side) => {
                let a = *self.vector_path(&layer)?.path.anchor(at).map_err(|e| e.code().to_string())?;
                let end = a.handle_point(side);
                PathEdit::MoveHandle { at, side, to: [end.x + dx - a.position.x, end.y + dy - a.position.y], constraint, break_mirroring }
            }
            DirectTarget::Segment { contour, segment, t } => PathEdit::Bend { contour, segment, t, dx, dy },
        };
        self.edit_path_inner(&layer, edit, "direct_select")
    }

    /// One path edit of the model surface on vector layer `layer_id` (one transaction; a live shape
    /// is expanded). Returns the layer id (unchanged).
    pub fn edit_path(&mut self, layer_id: &str, edit: PathEdit) -> Result<String, String> {
        let r = self.edit_path_inner(layer_id, edit, "edit_path");
        self.report(r)
    }

    fn edit_path_inner(&mut self, layer_id: &str, edit: PathEdit, op: &str) -> Result<String, String> {
        let record = self.vector_path(layer_id)?;
        let path = apply(&record.path, edit)?;
        self.replace_vector_layer(op, layer_id, path, None, None)
    }

    // ----- shapes -----------------------------------------------------------------------------------

    /// Shape tool: a live shape dragged from `a` to `b` (document pixels; polygon and star from the
    /// centre `a`; `constrain` = Shift). One transaction creating a vector layer with its Folio live
    /// shape record. Returns the layer id.
    pub fn draw_shape(&mut self, kind: ShapeKind, a: [f64; 2], b: [f64; 2], constrain: bool) -> Result<String, String> {
        let r = self.draw_shape_inner(kind, a, b, constrain);
        self.report(r)
    }

    fn draw_shape_inner(&mut self, kind: ShapeKind, a: [f64; 2], b: [f64; 2], constrain: bool) -> Result<String, String> {
        if !a.iter().chain(&b).all(|v| v.is_finite()) {
            return Err("shape: non_finite".into());
        }
        let unit = self.vdoc()?.geometry_unit;
        let (path, live) = shape_of(kind, &self.edit.vector, a, b, constrain, unit)?;
        let open = matches!(kind, ShapeKind::Line | ShapeKind::Spiral) || (kind == ShapeKind::ArcSegment && !self.edit.vector.arc_closed);
        let style = if open || matches!(kind, ShapeKind::RectangularGrid | ShapeKind::PolarGrid) { VectorStyle { fill: None, ..self.style_now(true) } } else { self.style_now(false) };
        let name = match kind {
            ShapeKind::Rectangle => "Rectangle",
            ShapeKind::RoundedRectangle => "Rounded Rectangle",
            ShapeKind::Ellipse => "Ellipse",
            ShapeKind::Polygon => "Polygon",
            ShapeKind::Star => "Star",
            ShapeKind::Line => "Line",
            ShapeKind::ArcSegment => "Arc",
            ShapeKind::RectangularGrid => "Rectangular Grid",
            ShapeKind::PolarGrid => "Polar Grid",
            ShapeKind::Spiral => "Spiral",
        };
        self.create_vector_layer("shape", name, path, style, Some(live))
    }

    /// Changes the parameters of a live shape (STU-VEC-010): new geometry from the same Nib builder,
    /// record kept live. One transaction. Returns the layer id (unchanged).
    pub fn set_live_shape(&mut self, layer_id: &str, params: StudioShapeParameters) -> Result<String, String> {
        let r = (|| {
            self.live_shape(layer_id)?.ok_or_else(|| format!("live_shape: {layer_id} is not a live shape"))?;
            let path = shape_geometry(&params)?;
            self.replace_vector_layer("live_shape", layer_id, path, None, Some(params))
        })();
        self.report(r)
    }

    /// Expands a live shape to plain geometry (STU-VEC-010: an explicit, history-tracked command).
    pub fn expand_shape(&mut self, layer_id: &str) -> Result<String, String> {
        let r = (|| {
            self.live_shape(layer_id)?.ok_or_else(|| format!("expand: {layer_id} is not a live shape"))?;
            let record = self.vector_path(layer_id)?;
            self.replace_vector_layer("expand_shape", layer_id, record.path, None, None)
        })();
        self.report(r)
    }

    // ----- geometry operations --------------------------------------------------------------------------

    /// The unified pathfinder (STU-VEC-130) over vector layers of one stack: operands bottom-first
    /// by stack order, all paths of a layer as one operand. One transaction: the result pieces become
    /// vector layers above the topmost operand (style of the topmost covering operand), the operands
    /// are deleted. Returns the new layer ids.
    pub fn pathfinder_layers(&mut self, op: PathfinderOp, layer_ids: &[String]) -> Result<Vec<String>, String> {
        let r = self.pathfinder_inner(op, layer_ids);
        self.report(r)
    }

    fn pathfinder_inner(&mut self, op: PathfinderOp, layer_ids: &[String]) -> Result<Vec<String>, String> {
        let mut operands: Vec<(u64, String, PathRecord)> = Vec::new();
        let mut parent: Option<Parent> = None;
        for id in layer_ids {
            let (p, index) = self.run.placement(id).ok_or_else(|| format!("pathfinder: {id} is not in a layer stack"))?;
            if parent.as_ref().is_some_and(|q| *q != p) {
                return Err("pathfinder: operands must share one parent".into());
            }
            parent = Some(p);
            if operands.iter().any(|(_, o, _)| o == id) {
                return Err(format!("pathfinder: {id} named twice"));
            }
            operands.push((index, id.clone(), self.vector_path(id)?));
        }
        operands.sort_by_key(|(i, _, _)| *i);
        let paths = operands.iter().map(|(_, _, r)| r.path.to_owned_path().map_err(|e| format!("pathfinder: {}", e.code()))).collect::<Result<Vec<_>, _>>()?;
        let keys: Vec<u64> = operands.iter().map(|(_, _, r)| style_key(&r.style)).collect();
        let result = pathfinder(op, &paths, Some(&keys), &BooleanOptions::default()).map_err(|e| format!("pathfinder {}: {e:?}", op.wire()))?;
        let parent = parent.ok_or("pathfinder: no operands")?;
        let top = operands.last().map(|(_, id, _)| id.clone()).ok_or("pathfinder: no operands")?;
        let mut entry = self.vector_entry()?;
        if entry.paths.len().saturating_add(result.pieces.len()) > MAX_PATHS {
            return Err("pathfinder: vector_entry_over_budget".into());
        }
        let doc = self.vdoc()?;
        let mut writes = Vec::new();
        let mut ids = Vec::new();
        let mut salt = u64::try_from(doc.layers.len()).unwrap_or(0);
        for piece in &result.pieces {
            let style = piece.sources.iter().max().and_then(|i| operands.get(*i)).map(|(_, _, r)| r.style).ok_or("pathfinder: piece without source")?;
            let path = AnchorPath::from_owned_path(&piece.path).map_err(|e| format!("pathfinder: {}", e.code()))?;
            let mut id = super::layers::mint_layer_id(doc, salt);
            while ids.contains(&id) {
                salt = salt.wrapping_add(1);
                id = super::layers::mint_layer_id(doc, salt);
            }
            salt = salt.wrapping_add(1);
            let path_id = mint_path_id(doc, &entry);
            entry.paths.push(PathRecord::plain(path_id.clone(), path, style));
            let layer = super::layers::new_layer(&id, op.member(), &parent, "vector", json!({"payload_kind": "vector", "path_ids": [path_id]}))?;
            writes.push(PlacementWrite { layer_id: id.clone(), to: Some(Place::Before(top.clone())), create: Some((layer, super::layers::operation_for(&id, false)?)) });
            ids.push(id);
        }
        writes.extend(operands.iter().map(|(_, id, _)| PlacementWrite { layer_id: id.clone(), to: None, create: None }));
        self.store_entry(&entry)?;
        self.structure("pathfinder", writes, format!("pathfinder {} -> {} piece(s)", op.wire(), ids.len()))?;
        self.selected = ids.first().cloned();
        Ok(ids)
    }

    /// Offset path (STU-VEC-006): a new vector layer above `layer_id` with its outline offset by
    /// `offset` (negative = inset) and the given join. One transaction. Returns the new layer id.
    pub fn offset_path_layer(&mut self, layer_id: &str, offset: f64, join: LineJoin) -> Result<String, String> {
        let r = (|| {
            let record = self.vector_path(layer_id)?;
            let owned = record.path.to_owned_path().map_err(|e| e.code().to_string())?;
            let result = hsk_studio_nib::edit::offset_path(&owned, offset, join, 4.0, &BooleanOptions::default()).map_err(|e| format!("offset_path: {e:?}"))?;
            let path = AnchorPath::from_owned_path(&result.path).map_err(|e| format!("offset_path: {}", e.code()))?;
            self.selected = Some(layer_id.to_string());
            self.create_vector_layer("offset_path", "Offset Path", path, record.style, None)
        })();
        self.report(r)
    }

    /// Outline stroke (STU-VEC-006): replaces the layer by the filled outline of its stroke (fill =
    /// the stroke colour, no stroke). One transaction. Returns the layer id (unchanged).
    pub fn outline_stroke_layer(&mut self, layer_id: &str) -> Result<String, String> {
        let r = (|| {
            let record = self.vector_path(layer_id)?;
            let colour = record.style.stroke.ok_or("outline_stroke: the layer has no stroke")?;
            let owned = record.path.to_owned_path().map_err(|e| e.code().to_string())?;
            let result = outline_stroke(&owned, &StrokeStyle::new(record.style.stroke_width), &StrokeOptions::default()).map_err(|e| format!("outline_stroke: {e:?}"))?;
            let path = AnchorPath::from_owned_path(&result.path).map_err(|e| format!("outline_stroke: {}", e.code()))?;
            self.replace_vector_layer("outline_stroke", layer_id, path, Some(VectorStyle { fill: Some(colour), stroke: None, stroke_width: 0.0 }), None)
        })();
        self.report(r)
    }

    // ----- canvas -------------------------------------------------------------------------------------

    /// Canvas gestures of the vector tools (called by the canvas with its document mapping).
    pub(super) fn vector_gesture(&mut self, ui: &egui::Ui, response: &egui::Response, to_doc: &dyn Fn(egui::Pos2) -> [f64; 2], zoom: f32) {
        let tool = self.edit.tool;
        let radius = HIT_RADIUS_SCREEN / f64::from(zoom.max(0.01));
        let (shift, alt) = ui.input(|i| (i.modifiers.shift, i.modifiers.alt));
        if let Some(p) = response.hover_pos() {
            self.edit.vector.current = Some(to_doc(p));
        }
        if tool == Tool::Pen && ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape)) {
            self.pen_end();
        }
        if response.drag_started()
            && let Some(p) = response.interact_pointer_pos()
        {
            self.edit.vector.press = Some(to_doc(p));
            self.edit.vector.samples.clear();
        }
        if response.dragged()
            && let Some(p) = response.interact_pointer_pos()
        {
            let d = to_doc(p);
            self.edit.vector.current = Some(d);
            if self.edit.vector.samples.len() < freeform::MAX_SAMPLES {
                self.edit.vector.samples.push(d);
            }
        }
        let constraint = if shift { Constraint::Angle45 } else { Constraint::Free };
        if response.drag_stopped()
            && let (Some(a), Some(b)) = (self.edit.vector.press.take(), self.edit.vector.current)
        {
            let mut samples = std::mem::take(&mut self.edit.vector.samples);
            samples.insert(0, a);
            let _ = match tool {
                Tool::Pen => self.pen_point(a[0], a[1], Some([b[0] - a[0], b[1] - a[1]]), radius),
                Tool::DirectSelect => self.direct_drag(a, b, constraint, alt, radius),
                Tool::Pencil => self.pencil_stroke(&samples),
                Tool::Knife => self.knife_cut(&samples).map(|ids| ids.join(",")),
                Tool::Width => self.width_drag(a, b, radius),
                Tool::Reshape => self.reshape_drag(a, b, radius),
                Tool::BlobBrush => self.blob_brush_stroke(&samples),
                Tool::ShapeBuilder => {
                    let operands = self.stack_vector_layers();
                    self.shape_builder_layers(&operands, &samples, alt).map(|ids| ids.join(","))
                }
                t => match t.shape() {
                    Some(kind) => self.draw_shape(kind, a, b, shift),
                    None => Ok(String::new()),
                },
            };
        }
        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            let d = to_doc(p);
            match tool {
                Tool::Pen => {
                    let _ = self.pen_point(d[0], d[1], None, radius);
                }
                Tool::Curvature => {
                    let _ = self.curvature_point(d[0], d[1], alt, radius);
                }
                Tool::ShapeBuilder => {
                    let operands = self.stack_vector_layers();
                    let _ = self.shape_builder_layers(&operands, &[d], alt);
                }
                Tool::DirectSelect => self.selected = self.direct_hit(d, radius).map(|(id, _)| id).or(self.selected.take()),
                Tool::PaintBucket => {
                    if let Some(layer) = self.selected.clone() {
                        let _ = self.network_paint_bucket(&layer, d, !alt);
                    } else {
                        self.say("paint bucket: select a vector layer first".into());
                    }
                }
                Tool::BlobBrush => {
                    let _ = self.blob_brush_stroke(&[d]);
                }
                _ => {}
            }
        }
    }

    /// Path overlay of the selected (or pen) vector layer: outline, anchors, handles, pen rubber band
    /// and the shape drag preview.
    pub(super) fn vector_overlay(&self, painter: &egui::Painter, to_screen: &dyn Fn([f32; 2]) -> egui::Pos2) {
        let blue = egui::Color32::from_rgb(40, 140, 255);
        let line = egui::Stroke::new(1.0_f32, blue);
        let tool = self.edit.tool;
        let shown = self.edit.vector.pen_layer.clone().or_else(|| self.selected.clone());
        #[allow(clippy::cast_possible_truncation)]
        let pt = |p: PathPoint| to_screen([p.x as f32, p.y as f32]);
        if let Some(record) = shown.and_then(|id| self.vector_path(&id).ok()) {
            for (ci, c) in record.path.contours.iter().enumerate() {
                for si in 0..c.segment_count() {
                    let pts: Vec<egui::Pos2> = (0..=24_u32).filter_map(|k| ap::point_at(&record.path, ci, si, f64::from(k) / 24.0).ok()).map(pt).collect();
                    painter.add(egui::Shape::line(pts, line));
                }
                for a in &c.anchors {
                    let at = pt(a.position);
                    for side in [HandleSide::In, HandleSide::Out] {
                        let h = a.handle_point(side);
                        if h != a.position {
                            painter.line_segment([at, pt(h)], line);
                            painter.circle_filled(pt(h), 3.0, blue);
                        }
                    }
                    painter.rect_filled(egui::Rect::from_center_size(at, egui::vec2(6.0, 6.0)), 0.0, egui::Color32::WHITE);
                    painter.rect_stroke(egui::Rect::from_center_size(at, egui::vec2(6.0, 6.0)), 0.0, line, egui::StrokeKind::Middle);
                }
            }
            if tool == Tool::Pen
                && let (Some(last), Some(cur)) = (record.path.contours.last().filter(|c| !c.closed).and_then(|c| c.anchors.last()), self.edit.vector.current)
            {
                #[allow(clippy::cast_possible_truncation)]
                painter.line_segment([pt(last.position), to_screen([cur[0] as f32, cur[1] as f32])], egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(40, 140, 255, 140)));
            }
        }
        if let (Some(kind), Some(a), Some(b)) = (tool.shape(), self.edit.vector.press, self.edit.vector.current)
            && let Ok((path, _)) = shape_of(kind, &self.edit.vector, a, b, false, GeometryUnit::Px)
        {
            for (ci, c) in path.contours.iter().enumerate() {
                for si in 0..c.segment_count() {
                    let pts: Vec<egui::Pos2> = (0..=24_u32).filter_map(|k| ap::point_at(&path, ci, si, f64::from(k) / 24.0).ok()).map(pt).collect();
                    painter.add(egui::Shape::line(pts, line));
                }
            }
        }
    }

    /// Vector tool options (fill, stroke, shape parameters, pen end, pathfinder on the selection).
    pub(super) fn vector_options_ui(&mut self, ui: &mut egui::Ui, drawn: &mut Vec<String>) {
        if !self.edit.tool.is_vector() {
            return;
        }
        let mut t = self.edit.vector.clone();
        ui.separator();
        ui.push_id(super::widget::id(widget::FILL_ON), |ui| ui.checkbox(&mut t.fill_on, "fill"));
        drawn.push(widget::FILL_ON.into());
        ui.push_id(super::widget::id(widget::FILL), |ui| ui.color_edit_button_srgb(&mut t.fill));
        drawn.push(widget::FILL.into());
        ui.push_id(super::widget::id(widget::STROKE_ON), |ui| ui.checkbox(&mut t.stroke_on, "stroke"));
        drawn.push(widget::STROKE_ON.into());
        ui.push_id(super::widget::id(widget::STROKE), |ui| ui.color_edit_button_srgb(&mut t.stroke));
        drawn.push(widget::STROKE.into());
        ui.push_id(super::widget::id(widget::STROKE_WIDTH), |ui| ui.add(egui::Slider::new(&mut t.stroke_width, 0.0..=100.0).text("width")));
        drawn.push(widget::STROKE_WIDTH.into());
        match self.edit.tool {
            Tool::RoundedRectangle => {
                ui.push_id(super::widget::id(widget::CORNER_RADIUS), |ui| ui.add(egui::Slider::new(&mut t.corner_radius, 0.0..=500.0).text("radius")));
                drawn.push(widget::CORNER_RADIUS.into());
            }
            Tool::Polygon => {
                ui.push_id(super::widget::id(widget::SIDES), |ui| ui.add(egui::Slider::new(&mut t.polygon_sides, 3..=100).text("sides")));
                drawn.push(widget::SIDES.into());
            }
            Tool::Star => {
                ui.push_id(super::widget::id(widget::POINTS), |ui| ui.add(egui::Slider::new(&mut t.star_points, 3..=50).text("points")));
                drawn.push(widget::POINTS.into());
                ui.push_id(super::widget::id(widget::INNER), |ui| ui.add(egui::Slider::new(&mut t.star_inner, 0.05..=1.0).text("inner")));
                drawn.push(widget::INNER.into());
            }
            Tool::Pen | Tool::Curvature => {
                if ui.push_id(super::widget::id(widget::PEN_END), |ui| ui.add_enabled(t.pen_layer.is_some(), egui::Button::new("End path"))).inner.on_hover_text("Enter / Escape").clicked() {
                    t.pen_layer = None;
                }
                drawn.push(widget::PEN_END.into());
            }
            Tool::Pencil => {
                ui.push_id(super::widget::id(widget::PENCIL_FIDELITY), |ui| ui.add(egui::Slider::new(&mut t.pencil_fidelity, 0..=4).text("fidelity")));
                drawn.push(widget::PENCIL_FIDELITY.into());
            }
            Tool::Reshape => {
                ui.push_id(super::widget::id(widget::RESHAPE_RADIUS), |ui| ui.add(egui::Slider::new(&mut t.reshape_radius, 1.0..=500.0).text("falloff")));
                drawn.push(widget::RESHAPE_RADIUS.into());
            }
            Tool::BlobBrush => {
                ui.push_id(super::widget::id(widget::BLOB_DIAMETER), |ui| ui.add(egui::Slider::new(&mut t.blob_diameter, 1.0..=200.0).text("size")));
                drawn.push(widget::BLOB_DIAMETER.into());
            }
            Tool::ArcSegment => {
                ui.push_id(super::widget::id(widget::ARC_SLOPE), |ui| ui.add(egui::Slider::new(&mut t.arc_slope, -100.0..=100.0).text("slope")));
                drawn.push(widget::ARC_SLOPE.into());
                ui.push_id(super::widget::id(widget::ARC_CLOSED), |ui| ui.checkbox(&mut t.arc_closed, "closed"));
                drawn.push(widget::ARC_CLOSED.into());
            }
            Tool::RectangularGrid => {
                ui.push_id(super::widget::id(widget::GRID_ROWS), |ui| ui.add(egui::Slider::new(&mut t.grid_rows, 0..=50).text("rows")));
                drawn.push(widget::GRID_ROWS.into());
                ui.push_id(super::widget::id(widget::GRID_COLUMNS), |ui| ui.add(egui::Slider::new(&mut t.grid_columns, 0..=50).text("columns")));
                drawn.push(widget::GRID_COLUMNS.into());
            }
            Tool::PolarGrid => {
                ui.push_id(super::widget::id(widget::POLAR_CONCENTRIC), |ui| ui.add(egui::Slider::new(&mut t.polar_concentric, 0..=50).text("concentric")));
                drawn.push(widget::POLAR_CONCENTRIC.into());
                ui.push_id(super::widget::id(widget::POLAR_RADIAL), |ui| ui.add(egui::Slider::new(&mut t.polar_radial, 0..=50).text("radial")));
                drawn.push(widget::POLAR_RADIAL.into());
            }
            Tool::Spiral => {
                ui.push_id(super::widget::id(widget::SPIRAL_DECAY), |ui| ui.add(egui::Slider::new(&mut t.spiral_decay, 5.0..=99.0).text("decay %")));
                drawn.push(widget::SPIRAL_DECAY.into());
                ui.push_id(super::widget::id(widget::SPIRAL_TURNS), |ui| ui.add(egui::Slider::new(&mut t.spiral_turns, 0.25..=20.0).text("turns")));
                drawn.push(widget::SPIRAL_TURNS.into());
            }
            _ => {}
        }
        let mut run_op = None;
        if self.edit.tool == Tool::DirectSelect {
            ui.push_id(super::widget::id(widget::PATHFINDER), |ui| {
                egui::ComboBox::from_label("pathfinder").selected_text("apply to selected + below").show_ui(ui, |ui| {
                    for op in [PathfinderOp::Union, PathfinderOp::SubtractFront, PathfinderOp::Intersect, PathfinderOp::Exclude, PathfinderOp::Divide, PathfinderOp::Trim, PathfinderOp::Merge, PathfinderOp::Crop] {
                        if ui.selectable_label(false, op.member()).clicked() {
                            run_op = Some(op);
                        }
                    }
                });
            });
            drawn.push(widget::PATHFINDER.into());
        }
        t.press = self.edit.vector.press;
        t.current = self.edit.vector.current;
        t.samples = std::mem::take(&mut self.edit.vector.samples);
        self.edit.vector = t;
        if let Some(op) = run_op {
            // The selected vector layer and the vector layer directly below it in its stack.
            let operands = self.selected.clone().and_then(|sel| {
                let (parent, _) = self.run.placement(&sel)?;
                let group = match &parent {
                    Parent::Root(()) => None,
                    Parent::Node(n) => Some(n.node_id.clone()),
                };
                let stack = super::layers::stack(self.vdoc().ok()?, group.as_deref());
                let at = stack.iter().position(|l| *l == sel)?;
                let below = stack.get(at.checked_sub(1)?)?.clone();
                Some(vec![below, sel])
            });
            match operands {
                Some(ops) => {
                    let _ = self.pathfinder_layers(op, &ops);
                }
                None => self.say("pathfinder: select a vector layer that has a vector layer below it".into()),
            }
        }
    }

    /// Structured vector state (part of the edit projection).
    pub fn vector_json(&self) -> Value {
        let t = &self.edit.vector;
        let selected = self.selected.as_deref().and_then(|id| self.vector_path(id).ok().map(|r| (id, r)));
        json!({
            "fill": t.fill_on.then_some(t.fill), "stroke": t.stroke_on.then_some(t.stroke), "stroke_width": t.stroke_width,
            "corner_radius": t.corner_radius, "polygon_sides": t.polygon_sides, "star_points": t.star_points, "star_inner": t.star_inner,
            "pen_layer": t.pen_layer,
            "selected_path": selected.map(|(id, r)| json!({
                "layer_id": id, "path_id": r.path_id, "path": path_json(&r.path),
                "point_types": r.path.contours.iter().map(|c| c.anchors.iter().map(|a| a.point_type() as u8).collect::<Vec<_>>()).collect::<Vec<_>>(),
                "live_shape": self.live_shape(id).ok().flatten().map(|s| serde_json::to_value(s).unwrap_or(Value::Null)),
                "paint_style": self.layer_paint_style(id).ok().map(|s| serde_json::to_value(s).unwrap_or(Value::Null)),
            })),
            "swatches": self.swatches().unwrap_or_default().into_iter().map(|(id, name)| json!({"style_id": id, "name": name})).collect::<Vec<_>>(),
            "pathfinder_ops": PathfinderOp::ALL.iter().map(|o| o.wire()).collect::<Vec<_>>(),
        })
    }
}

/// Merge key of a style (same fill, stroke and width = same key).
fn style_key(s: &VectorStyle) -> u64 {
    let rgb = |c: Option<[u8; 3]>| c.map_or(0x0100_0000, |c| u64::from_be_bytes([0, 0, 0, 0, 0, c[0], c[1], c[2]]));
    rgb(s.fill).rotate_left(40) ^ rgb(s.stroke).rotate_left(8) ^ s.stroke_width.to_bits()
}
