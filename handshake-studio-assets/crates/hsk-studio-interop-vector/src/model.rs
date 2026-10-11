//! Neutral proposal model of vector import. Plain data: it is a proposal for the host to adopt,
//! never a private vector or style store (ENG-DAG-FORBID-003).
use crate::error::{Result, VectorError};
use hsk_studio_folio::Snapshot;
use hsk_studio_nib::{Anchor, OwnedPath as GeometryPath, PathLimits, Winding};

/// Hostile-input budgets, all checked before the corresponding work is done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_bytes: usize,
    /// XML nodes, and also the element budget of the walk.
    pub max_nodes: u32,
    pub max_depth: usize,
    /// Path segments and shape anchors summed over the document.
    pub max_segments: usize,
    /// Decompression-bomb cap: largest SVGZ payload after inflating (gzip is never unwrapped
    /// twice). `max_bytes` bounds the input as given, compressed or not.
    pub max_decompressed_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            max_nodes: 20_000,
            max_depth: 32,
            max_segments: 500_000,
            max_decompressed_bytes: 32 * 1024 * 1024,
        }
    }
}

/// Import options. There is deliberately no switch that allows external fetching: no code path
/// in this crate opens a network connection, a file or a link target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImportPolicy {
    pub limits: Limits,
    /// Pixels per inch used for the CSS pixel to point conversion (96 gives 0.75 pt per px).
    pub pixels_per_inch: f64,
}

impl Default for ImportPolicy {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            pixels_per_inch: 96.0,
        }
    }
}

impl ImportPolicy {
    pub(crate) fn validate(&self) -> Result<()> {
        if !self.pixels_per_inch.is_finite() || self.pixels_per_inch <= 0.0 {
            return Err(VectorError::InvalidPolicy("pixels_per_inch"));
        }
        if self.limits.max_nodes == 0 || self.limits.max_depth == 0 || self.limits.max_segments == 0
        {
            return Err(VectorError::InvalidPolicy("limits"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LossKind {
    /// Geometry or value is a close but not exact representation (arc to cubic, ellipse cubics).
    Approximated,
    /// A `transform` was applied to the points and is no longer an editable attribute.
    TransformBaked,
    /// A feature of the source has no counterpart in the proposal and was not carried over.
    Unsupported,
    /// A reference to something outside the file was denied; nothing was fetched.
    ExternalDenied,
    /// Content without visual meaning (metadata, foreign namespaces, degenerate subpaths).
    Ignored,
}

impl LossKind {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Approximated => "approximated",
            Self::TransformBaked => "transform_baked",
            Self::Unsupported => "unsupported",
            Self::ExternalDenied => "external_denied",
            Self::Ignored => "ignored",
        }
    }
}

/// One reported loss. `path` is an element locator such as `/svg[1]/g[2]/path[1]`; it holds tag
/// names and ordinals only, never attribute values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LossEntry {
    pub path: String,
    pub kind: LossKind,
    pub code: &'static str,
}

/// Straight-alpha colour; `a` is the combined colour alpha and fill or stroke opacity, `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: f64,
}

/// `stroke_cap` wire values 1..3 (STU-VEC-143); SVG `square` is `Projecting`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokeCap {
    Butt,
    Round,
    Projecting,
}

impl StrokeCap {
    pub const fn wire(self) -> u8 {
        match self {
            Self::Butt => 1,
            Self::Round => 2,
            Self::Projecting => 3,
        }
    }
}

/// `stroke_join` wire values 1..3 (STU-VEC-143).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokeJoin {
    Miter,
    Round,
    Bevel,
}

impl StrokeJoin {
    pub const fn wire(self) -> u8 {
        match self {
            Self::Miter => 1,
            Self::Round => 2,
            Self::Bevel => 3,
        }
    }
}

/// Stroke geometry attributes; lengths are points after transform and unit conversion.
#[derive(Clone, Debug, PartialEq)]
pub struct StrokeStyle {
    pub width_pt: f64,
    pub cap: StrokeCap,
    pub join: StrokeJoin,
    /// Ratio, `1.0..=500.0` (STU-VEC-144); SVG default 4.
    pub miter_limit: f64,
    /// Alternating dash and gap lengths (STU-VEC-145); empty is a solid line. An odd SVG list is
    /// repeated once, which is what SVG defines.
    pub dash_pt: Vec<f64>,
    /// SVG `stroke-dashoffset`. UNVERIFIED: STU-VEC-145 lists no dash phase.
    pub dash_offset_pt: f64,
}

impl StrokeStyle {
    pub fn solid(width_pt: f64) -> Self {
        Self {
            width_pt,
            cap: StrokeCap::Butt,
            join: StrokeJoin::Miter,
            miter_limit: 4.0,
            dash_pt: Vec::new(),
            dash_offset_pt: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpreadMethod {
    Pad,
    Reflect,
    Repeat,
}

/// How the gradient geometry numbers were authored. The proposal's `transform` already includes
/// the bounding-box mapping, so this is provenance, not an extra input to rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradientUnits {
    ObjectBoundingBox,
    UserSpaceOnUse,
}

/// Straight-alpha stop; `offset` is `0.0..=1.0` and non-decreasing along the list. The colour
/// alpha already includes `stop-opacity` and the fill or stroke opacity of the painted element.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    pub offset: f64,
    pub color: Rgba,
}

/// Linear gradient from `(x1, y1)` to `(x2, y2)` in gradient space; `transform`
/// (`[a, b, c, d, e, f]`, point = `[a c e; b d f] * [x y 1]`) maps gradient space to document points.
#[derive(Clone, Debug, PartialEq)]
pub struct LinearGradient {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    pub units: GradientUnits,
    pub transform: [f64; 6],
    pub spread: SpreadMethod,
    pub stops: Vec<GradientStop>,
}

/// Radial gradient: circle `(cx, cy, r)` with focal circle `(fx, fy, fr)` in gradient space;
/// `transform` maps gradient space to document points.
#[derive(Clone, Debug, PartialEq)]
pub struct RadialGradient {
    pub cx: f64,
    pub cy: f64,
    pub r: f64,
    pub fx: f64,
    pub fy: f64,
    pub fr: f64,
    pub units: GradientUnits,
    pub transform: [f64; 6],
    pub spread: SpreadMethod,
    pub stops: Vec<GradientStop>,
}

/// Crate-local typed paint until Folio and the PAINT owner carry paint types (`StudioGradient`,
/// STU-RAS-160; fill kinds STU-VEC-134). Pattern paints are reported as losses.
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    Solid(Rgba),
    Linear(LinearGradient),
    Radial(RadialGradient),
}

#[derive(Clone, Debug, PartialEq)]
pub struct StrokeAppearance {
    pub paint: Paint,
    pub style: StrokeStyle,
}

/// One row of the appearance stack (STU-VEC-022).
#[derive(Clone, Debug, PartialEq)]
pub enum AppearanceItem {
    Fill(Paint),
    Stroke(StrokeAppearance),
}

/// Plain style data proposed for the PAINT style owner. The id is a proposed `SSTY` id that
/// [`VectorPath::fill_style_id`] refers to; it creates no style primitive. `items` is the
/// appearance stack from bottom to top: SVG expresses one fill and one stroke in either order
/// (`paint-order`); longer stacks come from callers and export as one path element per row.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleProposal {
    pub style_id: String,
    pub items: Vec<AppearanceItem>,
}

impl StyleProposal {
    pub fn fills(&self) -> impl Iterator<Item = &Paint> {
        self.items.iter().filter_map(|i| match i {
            AppearanceItem::Fill(p) => Some(p),
            AppearanceItem::Stroke(_) => None,
        })
    }

    pub fn strokes(&self) -> impl Iterator<Item = &StrokeAppearance> {
        self.items.iter().filter_map(|i| match i {
            AppearanceItem::Stroke(s) => Some(s),
            AppearanceItem::Fill(_) => None,
        })
    }
}

/// One subpath in the `hsk.studio.vector_path@1` shape: points are lengths in points, handles are
/// vectors relative to their anchor. A compound SVG path becomes several `VectorPath`s that share
/// `layer_id`.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorPath {
    /// `SVPT` id.
    pub path_id: String,
    /// `SLYR` id of the Folio layer that lists this path in its `Payload::Vector`.
    pub layer_id: String,
    pub closed: bool,
    pub anchors: Vec<Anchor>,
    /// `Winding::None` means the path has no fill.
    pub winding: Winding,
    /// Proposed `SSTY` id, if the source styled the shape.
    pub fill_style_id: Option<String>,
}

impl VectorPath {
    /// Geometry in points as Nib's owned path (fill rule `NonZero` when the path has no fill).
    pub fn to_geometry(&self, limits: &PathLimits) -> std::result::Result<GeometryPath, VectorError> {
        let winding = if self.winding == Winding::None {
            Winding::NonZero
        } else {
            self.winding
        };
        GeometryPath::from_anchors(
            &self.anchors,
            self.closed,
            winding,
            hsk_studio_accord::Unit::Points,
            None,
            limits,
        )
        .map_err(|_| VectorError::InvalidGeometry)
    }
}

/// Every unit conversion is recorded once here; coordinates in the proposal are already points.
#[derive(Clone, Debug, PartialEq)]
pub struct Provenance {
    pub source_format: &'static str,
    pub source_unit: &'static str,
    pub pixels_per_inch: f64,
    /// Points per source pixel (`72 / pixels_per_inch`).
    pub px_to_pt: f64,
    /// Length of the file as given (compressed for SVGZ).
    pub source_len: usize,
    /// Length of the SVG text that was parsed (equals `source_len` unless SVGZ).
    pub decoded_len: usize,
    /// `viewBox` as written, `[min_x, min_y, width, height]` in user units.
    pub view_box: Option<[f64; 4]>,
    /// Viewport size in source pixels.
    pub viewport_px: [f64; 2],
}

#[derive(Clone, Debug)]
pub struct VectorProposal {
    /// Folio document (revision 1) with one artboard, group layers and vector layers.
    pub snapshot: Snapshot,
    pub paths: Vec<VectorPath>,
    pub styles: Vec<StyleProposal>,
    pub loss: Vec<LossEntry>,
    pub provenance: Provenance,
}

impl VectorProposal {
    pub fn has_loss(&self, kind: LossKind) -> bool {
        self.loss.iter().any(|l| l.kind == kind)
    }
    pub fn has_loss_code(&self, code: &str) -> bool {
        self.loss.iter().any(|l| l.code == code)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExportOptions {
    pub pixels_per_inch: f64,
    /// Artboard size in points; `None` uses `[max x, max y]` of all anchors and handles (at
    /// least 1 pt). The `viewBox` always starts at `0 0`.
    pub artboard_pt: Option<[f64; 2]>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            pixels_per_inch: 96.0,
            artboard_pt: None,
        }
    }
}

/// Coordinates and handle components beyond this magnitude are refused (exact in binary64).
pub(crate) const COORDINATE_LIMIT: f64 = 1.0e9;
