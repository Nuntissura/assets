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
//! outline stroke) creates the successor layer directly above the old one and deletes the old one in
//! the same transaction: chronicle has no property for a vector layer's paths and refuses two
//! writes to one address in a patch, so the successor carries a new layer id (SPEC GAP, recorded
//! in the lane; the name, parent, stack slot, visibility and appearance are kept). Pathfinder
//! creates its result layers above the topmost operand and deletes the operands, in one
//! transaction; offset path creates a copy (STU-VEC-006 "numeric inset/outset copy").
//!
//! Rendering: each vector layer is drawn by composite's folio-free vector path
//! ([`hsk_studio_composite::prepare_vector_layers`]: nib exact coverage, nib strokes) and lowered
//! with `evaluate_with_inputs` / `lower_with_inputs` ([`super::render::render_full`]). Colours are
//! 8-bit sRGB in the entry and linear light in the plan (the plan's blend space).
//!
//! Studio choices (Illustrator parity UNVERIFIED, D5): new pen paths have no fill and a 2 px stroke
//! in the brush colour; shapes are filled with the brush colour and have no stroke; the pen closes
//! a path when the click lands within the hit radius of its first anchor; a direct edit of a live
//! shape expands it (its record is not carried to the successor).
#![deny(clippy::arithmetic_side_effects)]
use super::Viewer;
use super::edit::Tool;
use crate::{Place, PlacementWrite};
use hsk_studio_composite::{FillPaint, FillSpec, PreparedVectors, VectorGeometry, VectorLayerSpec, VectorResolution, prepare_vector_layers};
use hsk_studio_folio::{self as folio, GeometryUnit, Length, Parent, Payload, StudioDocument, StudioLiveShape, StudioLiveShapes, StudioShapeParameters};
use hsk_studio_nib::anchor_path::{
    self as ap, AnchorAddr, AnchorPath, AnchorSelection, Constraint, Contour, EditAnchor, EndAddr, HandleSide,
};
use hsk_studio_nib::boolean::BooleanOptions;
use hsk_studio_nib::pathfinder::{PathfinderOp, pathfinder};
use hsk_studio_nib::shape::{self, CornerKind, EllipseSpec, RectSpec};
use hsk_studio_nib::stroke::{LineJoin, StrokeOptions, StrokeStyle, outline_stroke};
use hsk_studio_nib::{FillRule, Mirroring, PathPoint};
use hsk_studio_pigment::blend_math::StudioBlendMode;
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
}

/// The `vector` document entry.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorEntry {
    pub paths: Vec<PathRecord>,
    /// Live shape records (Folio `StudioLiveShape`); only those whose layer and path are in the
    /// document are validated and offered (the others belong to revisions the history can return to).
    pub live: Vec<StudioLiveShape>,
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
    /// The layer the pen extends (`None` = the next click starts a new path).
    pub pen_layer: Option<String>,
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
            pen_layer: None,
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
        Self { paths: Vec::new(), live: Vec::new() }
    }

    pub fn path(&self, path_id: &str) -> Option<&PathRecord> {
        self.paths.iter().find(|p| p.path_id == path_id)
    }

    /// Strict decode: known fields only, schema, budgets (paths, anchors) before allocation, every
    /// path validated by Nib, styles in range; then [`VectorEntry::validate`].
    pub fn decode(bytes: &[u8], doc: &StudioDocument, cancel: &hsk_studio_accord::CancellationToken) -> Result<Self, String> {
        let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("vector_entry_json: {e}"))?;
        let object = v.as_object().ok_or("vector_entry_json: not an object")?;
        if object.keys().any(|k| !["schema", "paths", "live_shapes"].contains(&k.as_str())) {
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
            if o.keys().any(|k| !["path_id", "fill", "stroke", "stroke_width", "path"].contains(&k.as_str())) {
                return Err("vector_entry_unknown_field".into());
            }
            records.push(PathRecord {
                path_id: p["path_id"].as_str().ok_or("vector_entry_path_id")?.to_string(),
                style: VectorStyle { fill: rgb_of(&p["fill"], "fill")?, stroke: rgb_of(&p["stroke"], "stroke")?, stroke_width: p["stroke_width"].as_f64().ok_or("vector_entry_style: stroke_width")? },
                path: path_of(&p["path"], &mut budget)?,
            });
        }
        let live: StudioLiveShapes = serde_json::from_value(v["live_shapes"].clone()).map_err(|e| format!("vector_entry_live_shapes: {e}"))?;
        if live.document_id != doc.document_id {
            return Err("vector_entry_document".into());
        }
        let entry = Self { paths: records, live: live.shapes };
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
            .map(|p| json!({"path_id": p.path_id, "fill": rgb_json(p.style.fill), "stroke": rgb_json(p.style.stroke), "stroke_width": p.style.stroke_width, "path": path_json(&p.path)}))
            .collect();
        let mut live = StudioLiveShapes::new(document_id);
        live.shapes.clone_from(&self.live);
        let v = json!({"schema": ENTRY_SCHEMA, "paths": paths, "live_shapes": live});
        serde_json::to_vec(&v).map_err(|e| format!("vector_entry_json: {e}"))
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

/// IEC 61966-2-1 decode of an 8-bit sRGB value into linear light.
fn linear(v: u8) -> f32 {
    let c = f32::from(v) / 255.0;
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn solid(c: [u8; 3]) -> FillPaint {
    FillPaint::Solid([linear(c[0]), linear(c[1]), linear(c[2]), 1.0])
}

/// Composite specs of every vector layer of `doc` (paths resolved from `entry`; a layer whose path
/// the entry lacks is drawn without that path and reported by the caller's render notes).
pub fn specs(doc: &StudioDocument, entry: &VectorEntry) -> Result<Vec<VectorLayerSpec>, String> {
    let mut out = Vec::new();
    for layer in &doc.layers {
        let Payload::Vector { path_ids } = &layer.payload else { continue };
        let mut paths = Vec::with_capacity(path_ids.len());
        let mut style = None;
        for id in path_ids {
            let record = entry.path(id).ok_or_else(|| format!("vector_path_unresolved: {} {id}", layer.layer_id))?;
            paths.push(record.path.to_owned_path().map_err(|e| format!("vector_path: {id} {}", e.code()))?);
            style.get_or_insert(record.style);
        }
        let style = style.unwrap_or(VectorStyle { fill: None, stroke: None, stroke_width: 0.0 });
        let mut fills = Vec::new();
        if let Some(c) = style.fill {
            fills.push(FillSpec { paint: solid(c), mode: StudioBlendMode::Normal, opacity: 1.0 });
        }
        if let Some(c) = style.stroke
            && style.stroke_width > 0.0
        {
            fills.push(FillSpec { paint: FillPaint::Stroke { style: StrokeStyle::new(style.stroke_width), paint: Box::new(solid(c)) }, mode: StudioBlendMode::Normal, opacity: 1.0 });
        }
        out.push(VectorLayerSpec { layer_id: layer.layer_id.clone(), geometry: VectorGeometry::Paths(paths), fills });
    }
    Ok(out)
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
            _ => None,
        }
    }
    /// Pen, direct selection or a shape tool.
    pub fn is_vector(self) -> bool {
        matches!(self, Tool::Pen | Tool::DirectSelect) || self.shape().is_some()
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
        let (press, current, pen) = (self.edit.vector.press, self.edit.vector.current, self.edit.vector.pen_layer.take());
        self.edit.vector = VectorTool { press, current, pen_layer: pen, ..tool };
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

    /// Prepared vector layers of the open document (`None` without vector layers).
    pub(super) fn prepared_vectors(&self) -> Result<Option<PreparedVectors>, String> {
        let doc = self.vdoc()?;
        if !doc.layers.iter().any(|l| matches!(l.payload, Payload::Vector { .. })) {
            return Ok(None);
        }
        let specs = specs(doc, &self.vector_entry()?)?;
        prepare_vector_layers(&specs, &VectorResolution::default(), self.cancel).map(Some).map_err(|e| format!("vector_prepare: {e:?}"))
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
        entry.paths.push(PathRecord { path_id: path_id.clone(), path, style });
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

    /// The successor of vector layer `old` with `path` (and `live`, when the shape stays live): one
    /// transaction creating it directly above `old` and deleting `old`; selects it.
    fn replace_vector_layer(&mut self, op: &str, old: &str, path: AnchorPath, style: Option<VectorStyle>, live: Option<StudioShapeParameters>) -> Result<String, String> {
        path.validate().map_err(|e| format!("{op}: {}", e.code()))?;
        let record = self.vector_path(old)?;
        let doc = self.vdoc()?;
        let old_layer = doc.layers.iter().find(|l| l.layer_id == old).cloned().ok_or_else(|| format!("no layer {old}"))?;
        self.run.placement(old).ok_or_else(|| format!("{op}: {old} is not in a layer stack"))?;
        let mut entry = self.vector_entry()?;
        if entry.paths.len() >= MAX_PATHS {
            return Err(format!("{op}: vector_entry_over_budget"));
        }
        let id = super::layers::mint_layer_id(doc, u64::try_from(doc.layers.len()).unwrap_or(0));
        let path_id = mint_path_id(doc, &entry);
        entry.paths.push(PathRecord { path_id: path_id.clone(), path, style: style.unwrap_or(record.style) });
        if let Some(shape) = live {
            entry.live.push(StudioLiveShape { layer_id: id.clone(), path_id: path_id.clone(), shape });
        }
        let mut layer = old_layer;
        layer.layer_id.clone_from(&id);
        layer.payload = Payload::Vector { path_ids: vec![path_id] };
        let operation = super::layers::operation_for(&id, false)?;
        self.store_entry(&entry)?;
        let writes = vec![
            PlacementWrite { layer_id: id.clone(), to: Some(Place::Before(old.to_string())), create: Some((layer, operation)) },
            PlacementWrite { layer_id: old.to_string(), to: None, create: None },
        ];
        self.structure(op, writes, format!("{op} {old} -> {id}"))?;
        if self.edit.vector.pen_layer.as_deref() == Some(old) {
            self.edit.vector.pen_layer = Some(id.clone());
        }
        self.selected = Some(id.clone());
        Ok(id)
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

    /// Ends the pen path (the next pen click starts a new one). No transaction.
    pub fn pen_end(&mut self) {
        self.edit.vector.pen_layer = None;
        self.edit.vector.press = None;
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
    /// transaction; a live shape is expanded. Returns the successor layer id.
    pub fn direct_drag(&mut self, from: [f64; 2], to: [f64; 2], constraint: Constraint, break_mirroring: bool, radius: f64) -> Result<String, String> {
        let r = self.direct_drag_inner(from, to, constraint, break_mirroring, radius);
        self.report(r)
    }

    fn direct_drag_inner(&mut self, from: [f64; 2], to: [f64; 2], constraint: Constraint, break_mirroring: bool, radius: f64) -> Result<String, String> {
        if !from.iter().chain(&to).all(|v| v.is_finite()) || !radius.is_finite() {
            return Err("direct_select: non_finite".into());
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
    /// is expanded). Returns the successor layer id.
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
        let style = if kind == ShapeKind::Line { VectorStyle { fill: None, ..self.style_now(true) } } else { self.style_now(false) };
        let name = match kind {
            ShapeKind::Rectangle => "Rectangle",
            ShapeKind::RoundedRectangle => "Rounded Rectangle",
            ShapeKind::Ellipse => "Ellipse",
            ShapeKind::Polygon => "Polygon",
            ShapeKind::Star => "Star",
            ShapeKind::Line => "Line",
        };
        self.create_vector_layer("shape", name, path, style, Some(live))
    }

    /// Changes the parameters of a live shape (STU-VEC-010): new geometry from the same Nib builder,
    /// record kept live. One transaction. Returns the successor layer id.
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
            entry.paths.push(PathRecord { path_id: path_id.clone(), path, style });
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
    /// the stroke colour, no stroke). One transaction. Returns the successor layer id.
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
        }
        if response.dragged()
            && let Some(p) = response.interact_pointer_pos()
        {
            self.edit.vector.current = Some(to_doc(p));
        }
        let constraint = if shift { Constraint::Angle45 } else { Constraint::Free };
        if response.drag_stopped()
            && let (Some(a), Some(b)) = (self.edit.vector.press.take(), self.edit.vector.current)
        {
            let _ = match tool {
                Tool::Pen => self.pen_point(a[0], a[1], Some([b[0] - a[0], b[1] - a[1]]), radius),
                Tool::DirectSelect => self.direct_drag(a, b, constraint, alt, radius),
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
                Tool::DirectSelect => self.selected = self.direct_hit(d, radius).map(|(id, _)| id).or(self.selected.take()),
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
            Tool::Pen => {
                if ui.push_id(super::widget::id(widget::PEN_END), |ui| ui.add_enabled(t.pen_layer.is_some(), egui::Button::new("End path"))).inner.on_hover_text("Enter / Escape").clicked() {
                    t.pen_layer = None;
                }
                drawn.push(widget::PEN_END.into());
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
            })),
            "pathfinder_ops": PathfinderOp::ALL.iter().map(|o| o.wire()).collect::<Vec<_>>(),
        })
    }
}

/// Merge key of a style (same fill, stroke and width = same key).
fn style_key(s: &VectorStyle) -> u64 {
    let rgb = |c: Option<[u8; 3]>| c.map_or(0x0100_0000, |c| u64::from_be_bytes([0, 0, 0, 0, 0, c[0], c[1], c[2]]));
    rgb(s.fill).rotate_left(40) ^ rgb(s.stroke).rotate_left(8) ^ s.stroke_width.to_bits()
}
