//! `StudioLiveShapes` (lane SMB-ILLUSTRATOR-PEN slice 2): the editable parameters of parametric
//! shapes (STU-VEC-010: "Each shape is a live `StudioVectorPath`/`StudioVectorNetwork` whose
//! defining parameters remain editable until the operator or model explicitly expands it to raw
//! geometry"; catalog and bounds STU-VEC-127; corner kinds STU-VEC-112; per-corner radius
//! STU-VEC-066). One record names a `vector` layer, the `SVPT` path of that layer that the shape's
//! geometry is, and the shape kind with its parameters; the geometry itself stays an ordinary
//! vector path (Nib builds it: `hsk_studio_nib::shape`), so a consumer that ignores this set still
//! draws the shape. Folio keeps the set beside the document like `StudioVectorRecords`, because a
//! new `Payload` variant would break every exhaustive match on it. Expansion (STU-VEC-010) is
//! removing the layer's record; the caller records that as a history step.
//!
//! Declared gaps, not invented here:
//! - The spec gives no schema id for the live-shape parameters; `hsk.studio.live_shapes@1` is this
//!   set's own token, like `hsk.studio.vector_records@1`.
//! - STU-VEC-127 states the rectangle bound `0..=16348` in `pt`; it is applied when the document
//!   unit is `pt` and otherwise only the physical domain (non-negative) is checked, because Folio
//!   has no resolution to convert another unit.
//! - Bounds marked `unknown` (angles, inner ratio, star inner radius, line length and angle,
//!   radii) are checked for finiteness and physical domain only: `inner_ratio` `0..1`, star
//!   `inner_radius` positive, lengths non-negative.
//! - Nine shapes are carried: rectangle, ellipse/arc, polygon, star, line, and (lane
//!   SMB-ILLUSTRATOR-PEN-2) arc segment, rectangular grid, polar grid and spiral; divider counts
//!   and spiral quarter turns are bounded by Studio budgets (the spec captures no bound).
//! - `corner_kind` `fancy` is stored (it is a captured member) but Nib refuses to build it, and
//!   `corner_smoothing` is stored as a percent `>= 0` (no captured maximum).
//!
//! The set arrives from untrusted wire JSON: integer arithmetic is denied (workflow V4-01) and the
//! shape count is bounded by `Budget::nodes` before the shapes are walked.
#![deny(clippy::arithmetic_side_effects)]
use super::{Budget, Code, GeometryUnit, Length, Payload, Result, StudioDocument, canceled, err, finite_length, id};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

schema_token!(LiveShapesSchema, "hsk.studio.live_shapes@1");

/// STU-VEC-127 rectangle width/height hard maximum (pt).
pub const LIVE_RECT_MAX_PT: f64 = 16348.0;
/// STU-VEC-127 polygon side count, normative hard range.
pub const LIVE_POLYGON_SIDES: (u32, u32) = (3, 100);
/// STU-VEC-127 star point count hard range.
pub const LIVE_STAR_POINTS: (u32, u32) = (3, 50);
/// Most dividers of one grid direction (a Studio budget: the spec captures no bound).
pub const LIVE_MAX_DIVIDERS: u32 = 1000;
/// Most quarter turns of a spiral (a Studio budget: the spec captures no bound).
pub const LIVE_MAX_SPIRAL_QUARTERS: u32 = 4096;

/// STU-VEC-112 corner kinds (serialised as the Studio member names; the captured values are
/// [`StudioCornerKind::capture_value`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioCornerKind {
    None,
    Round,
    RoundInverted,
    Inset,
    Bevel,
    Fancy,
}
impl StudioCornerKind {
    pub const fn capture_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Round => "rounded corner",
            Self::RoundInverted => "inverse rounded corner",
            Self::Inset => "inset corner",
            Self::Bevel => "bevel corner",
            Self::Fancy => "fancy corner",
        }
    }
}

/// The shape kind and its parameters (tag `shape_kind`); lengths in the document unit, angles in
/// degrees.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "shape_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioShapeParameters {
    Rectangle {
        x: Length,
        y: Length,
        width: Length,
        height: Length,
        /// `[top_left, top_right, bottom_right, bottom_left]`, each `>= 0` (STU-VEC-066).
        corner_radius: [Length; 4],
        corner_kind: StudioCornerKind,
        /// Percent, `>= 0`.
        corner_smoothing: f64,
    },
    Ellipse {
        center_x: Length,
        center_y: Length,
        radius_x: Length,
        radius_y: Length,
        start_angle: f64,
        end_angle: f64,
        /// `0..1`: 0 = ellipse or pie, above 0 = ring or segment.
        inner_ratio: f64,
    },
    Polygon {
        center_x: Length,
        center_y: Length,
        radius: Length,
        side_count: u32,
        /// Percent `0..=100`.
        inset_percentage: f64,
    },
    Star {
        center_x: Length,
        center_y: Length,
        /// The object's own size (outer radius).
        radius: Length,
        point_count: u32,
        /// Ratio of the outer radius, positive.
        inner_radius: f64,
    },
    Line {
        x: Length,
        y: Length,
        length: Length,
        angle: f64,
    },
    /// Arc segment (lane SMB-ILLUSTRATOR-PEN-2): from start to end; `slope` percent (soft
    /// -100..=100); `closed` and `base_along_x` are the Studio reading of the captured `type` and
    /// `base_along` popups whose members are UNKNOWN; `fill_arc` (default false).
    ArcSegment {
        start_x: Length,
        start_y: Length,
        end_x: Length,
        end_y: Length,
        slope: f64,
        closed: bool,
        base_along_x: bool,
        fill_arc: bool,
    },
    /// Rectangular grid: divider counts (at most [`LIVE_MAX_DIVIDERS`]), skews in percent (soft
    /// -500..=500), `use_outside_rectangle_as_frame` (default true), `fill_grid` (default true).
    RectangularGrid {
        x: Length,
        y: Length,
        width: Length,
        height: Length,
        horizontal_dividers: u32,
        vertical_dividers: u32,
        skew_horizontal: f64,
        skew_vertical: f64,
        use_outside_rectangle_as_frame: bool,
        fill_grid: bool,
    },
    /// Polar grid: concentric and radial divider counts, skews in percent,
    /// `create_compound_path_from_ellipses` (default false), `fill_grid` (default true).
    PolarGrid {
        center_x: Length,
        center_y: Length,
        radius_x: Length,
        radius_y: Length,
        concentric_dividers: u32,
        radial_dividers: u32,
        skew_radial: f64,
        skew_angular: f64,
        create_compound_path_from_ellipses: bool,
        fill_grid: bool,
    },
    /// Spiral: `decay` = the per-turn radius ratio in percent (positive), `turns` positive with at
    /// most [`LIVE_MAX_SPIRAL_QUARTERS`] quarter turns, winding `clockwise` on a y-down canvas.
    Spiral {
        center_x: Length,
        center_y: Length,
        radius: Length,
        decay: f64,
        turns: f64,
        clockwise: bool,
    },
}

impl StudioShapeParameters {
    /// The Studio shape name of STU-VEC-127.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Rectangle { .. } => "rectangle",
            Self::Ellipse { .. } => "ellipse",
            Self::Polygon { .. } => "polygon",
            Self::Star { .. } => "star",
            Self::Line { .. } => "line",
            Self::ArcSegment { .. } => "arc_segment",
            Self::RectangularGrid { .. } => "rectangular_grid",
            Self::PolarGrid { .. } => "polar_grid",
            Self::Spiral { .. } => "spiral",
        }
    }
}

/// One live shape: the vector layer, its path that is the shape's geometry, and the parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLiveShape {
    /// `SLYR` id of a layer with a `vector` payload.
    pub layer_id: String,
    /// `SVPT` id, one of the layer's `path_ids`.
    pub path_id: String,
    pub shape: StudioShapeParameters,
}

/// The live-shape set of one document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLiveShapes {
    pub schema_id: LiveShapesSchema,
    pub document_id: String,
    pub shapes: Vec<StudioLiveShape>,
}

impl StudioLiveShapes {
    pub fn new(document_id: &str) -> Self {
        Self { schema_id: LiveShapesSchema::V1, document_id: document_id.into(), shapes: Vec::new() }
    }
    /// The record of a layer.
    pub fn shape(&self, layer_id: &str) -> Option<&StudioLiveShape> {
        self.shapes.iter().find(|s| s.layer_id == layer_id)
    }
}

/// JSON schema (draft 2020-12) of the live-shape set.
pub fn live_shapes_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12().into_generator().into_root_schema_for::<StudioLiveShapes>()
}

fn field(ok: bool, target: &str, name: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(err(Code::InvalidField, format!("{target}/{name}"))) }
}

fn length(l: &Length, unit: GeometryUnit, size: bool, target: &str, name: &str) -> Result<()> {
    finite_length(l, unit, size, &format!("{target}/{name}"))
}

fn angle(v: f64, target: &str, name: &str) -> Result<()> {
    field(v.is_finite(), target, name)
}

fn validate_shape(s: &StudioShapeParameters, unit: GeometryUnit, t: &str) -> Result<()> {
    match s {
        StudioShapeParameters::Rectangle { x, y, width, height, corner_radius, corner_smoothing, .. } => {
            length(x, unit, false, t, "x")?;
            length(y, unit, false, t, "y")?;
            length(width, unit, true, t, "width")?;
            length(height, unit, true, t, "height")?;
            if unit == GeometryUnit::Pt {
                field(width.value <= LIVE_RECT_MAX_PT, t, "width")?;
                field(height.value <= LIVE_RECT_MAX_PT, t, "height")?;
            }
            for r in corner_radius {
                length(r, unit, true, t, "corner_radius")?;
            }
            field(corner_smoothing.is_finite() && *corner_smoothing >= 0.0, t, "corner_smoothing")
        }
        StudioShapeParameters::Ellipse { center_x, center_y, radius_x, radius_y, start_angle, end_angle, inner_ratio } => {
            length(center_x, unit, false, t, "center_x")?;
            length(center_y, unit, false, t, "center_y")?;
            length(radius_x, unit, true, t, "radius_x")?;
            length(radius_y, unit, true, t, "radius_y")?;
            angle(*start_angle, t, "start_angle")?;
            angle(*end_angle, t, "end_angle")?;
            field(inner_ratio.is_finite() && (0.0..1.0).contains(inner_ratio), t, "inner_ratio")
        }
        StudioShapeParameters::Polygon { center_x, center_y, radius, side_count, inset_percentage } => {
            length(center_x, unit, false, t, "center_x")?;
            length(center_y, unit, false, t, "center_y")?;
            length(radius, unit, true, t, "radius")?;
            field((LIVE_POLYGON_SIDES.0..=LIVE_POLYGON_SIDES.1).contains(side_count), t, "side_count")?;
            field(inset_percentage.is_finite() && (0.0..=100.0).contains(inset_percentage), t, "inset_percentage")
        }
        StudioShapeParameters::Star { center_x, center_y, radius, point_count, inner_radius } => {
            length(center_x, unit, false, t, "center_x")?;
            length(center_y, unit, false, t, "center_y")?;
            length(radius, unit, true, t, "radius")?;
            field((LIVE_STAR_POINTS.0..=LIVE_STAR_POINTS.1).contains(point_count), t, "point_count")?;
            field(inner_radius.is_finite() && *inner_radius > 0.0, t, "inner_radius")
        }
        StudioShapeParameters::Line { x, y, length: l, angle: a } => {
            length(x, unit, false, t, "x")?;
            length(y, unit, false, t, "y")?;
            length(l, unit, true, t, "length")?;
            angle(*a, t, "angle")
        }
        StudioShapeParameters::ArcSegment { start_x, start_y, end_x, end_y, slope, .. } => {
            for (l, name) in [(start_x, "start_x"), (start_y, "start_y"), (end_x, "end_x"), (end_y, "end_y")] {
                length(l, unit, false, t, name)?;
            }
            // Only a soft range is captured: the value must be finite.
            angle(*slope, t, "slope")
        }
        StudioShapeParameters::RectangularGrid { x, y, width, height, horizontal_dividers, vertical_dividers, skew_horizontal, skew_vertical, .. } => {
            length(x, unit, false, t, "x")?;
            length(y, unit, false, t, "y")?;
            length(width, unit, true, t, "width")?;
            length(height, unit, true, t, "height")?;
            field(*horizontal_dividers <= LIVE_MAX_DIVIDERS, t, "horizontal_dividers")?;
            field(*vertical_dividers <= LIVE_MAX_DIVIDERS, t, "vertical_dividers")?;
            angle(*skew_horizontal, t, "skew_horizontal")?;
            angle(*skew_vertical, t, "skew_vertical")
        }
        StudioShapeParameters::PolarGrid { center_x, center_y, radius_x, radius_y, concentric_dividers, radial_dividers, skew_radial, skew_angular, .. } => {
            length(center_x, unit, false, t, "center_x")?;
            length(center_y, unit, false, t, "center_y")?;
            length(radius_x, unit, true, t, "radius_x")?;
            length(radius_y, unit, true, t, "radius_y")?;
            field(*concentric_dividers <= LIVE_MAX_DIVIDERS, t, "concentric_dividers")?;
            field(*radial_dividers <= LIVE_MAX_DIVIDERS, t, "radial_dividers")?;
            angle(*skew_radial, t, "skew_radial")?;
            angle(*skew_angular, t, "skew_angular")
        }
        StudioShapeParameters::Spiral { center_x, center_y, radius, decay, turns, .. } => {
            length(center_x, unit, false, t, "center_x")?;
            length(center_y, unit, false, t, "center_y")?;
            length(radius, unit, true, t, "radius")?;
            field(decay.is_finite() && *decay > 0.0, t, "decay")?;
            field(turns.is_finite() && *turns > 0.0 && *turns * 4.0 <= f64::from(LIVE_MAX_SPIRAL_QUARTERS), t, "turns")
        }
    }
}

/// Validates a live-shape set against its document: same document, at most `b.nodes` shapes, each
/// naming an existing `vector` layer (at most one record per layer) and one of its `SVPT` paths,
/// lengths in the document unit, STU-VEC-127 bounds as described in the module docs.
pub fn validate_live_shapes(r: &StudioLiveShapes, d: &StudioDocument, b: Budget, token: &CancellationToken) -> Result<()> {
    canceled(token)?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if r.shapes.len() > b.nodes {
        return Err(err(Code::BudgetExceeded, "shapes"));
    }
    let mut seen = BTreeSet::new();
    for s in &r.shapes {
        canceled(token)?;
        let t = s.layer_id.as_str();
        id(t, "SLYR", "shapes/layer_id")?;
        id(&s.path_id, "SVPT", &format!("{t}/path_id"))?;
        if !seen.insert(t) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        let layer = d.layers.iter().find(|l| l.layer_id == t).ok_or_else(|| err(Code::DanglingReference, t))?;
        let Payload::Vector { path_ids } = &layer.payload else {
            return Err(err(Code::PayloadMismatch, t));
        };
        if !path_ids.contains(&s.path_id) {
            return Err(err(Code::DanglingReference, format!("{t}/path_id")));
        }
        validate_shape(&s.shape, d.geometry_unit, t)?;
    }
    Ok(())
}
