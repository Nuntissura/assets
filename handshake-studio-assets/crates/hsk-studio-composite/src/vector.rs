//! Vector layers with solid and image fills, and live boolean groups (lane SMB-COMPOSITE-NEW-KINDS
//! slices 2-3). Like text (`crate::text`), a vector layer is drawn on the host into one `coverage_u8`
//! plane and lowered through existing pigment routes only, so both backends render it without a new
//! operator: the layer's content is an isolated group `[Layer { rows, Normal, mask: coverage }]`
//! whose `rows` group holds one layer per fill row, bottom first, with the row's blend mode and
//! opacity: a solid row is a `Solid` over the plane, an image row (Folio `StudioImagePaint`) is the
//! caller-decoded image as a `Tile` placed by pigment's layer transform (`LayerExtension.transform`,
//! bilinear, transparent edges; one tile layer per copy for `TILE`).
//!
//! Coverage (Studio rule, class Approximate like render-cpu's vector coverage): each pixel is sampled
//! on an `n x n` grid of sample centres (`VectorResolution::aa_samples`); a sample is inside a path
//! by its exact winding (kurbo, the curve itself, open subpaths closed by a straight segment) and
//! the path's fill rule; coverage = `round(255 * hits / n^2)`. A layer's paths are united per sample.
//! A boolean group ([`VectorGeometry::Boolean`], Folio `StudioBooleanGroup`, STU-VEC-013) combines its
//! operands per sample: UNION = any, INTERSECT = all, SUBTRACT = the first and none of the others,
//! EXCLUDE = an odd number; Folio's cached outline (`fill_geometry_path_ids`) is used instead of the
//! operands when present. Sample-level booleans are exact at the sample resolution (no coverage
//! arithmetic between operands).
//!
//! Image placement (Folio convention: the transform maps the unit image square to the unit square of
//! the filled object's bounds, the geometry's bounding box): `CROP` applies the transform; `FILL`
//! (cover) and `FIT` (contain) scale the image uniformly about the bounds centre; `TILE` repeats the
//! image at `scaling_factor` from the bounds origin. `FILL`, `FIT` and `TILE` need the identity
//! transform and rotation 0 (`image_paint_transform`; Figma's rotation of a filled image is not
//! modelled). Figma's own placement maths is UNVERIFIED (D5): class Approximate.
//!
//! Strokes, gradients, patterns and exact coverage (lane SMB-COMPOSITE-VECTOR-IN-GROUP slice 2) come
//! from nib (`crate::vector_nib`): a stroke row ([`FillPaint::Stroke`]) paints inside the nib stroke
//! outline of the geometry (its own `coverage_u8` plane, [`StrokePlane`]); strokes paint above every
//! fill. A gradient or pattern row ([`FillPaint::Gradient`], [`FillPaint::Pattern`]) is evaluated by
//! nib over the layer plane into a straight RGBA tile ([`PreparedPaint::Painted`]). With
//! [`VectorCoverage::Exact`] the fill coverage is nib's exact area coverage of the layer outline
//! (paths united, boolean groups combined by nib) instead of the supersampled rule; Exact is the
//! default (lead decision after SMB-INTEROP-FIG-PAINTS b160fc3 moved its pins to Exact), and
//! `Supersampled` stays selectable (it also keeps the Folio adapter's earlier stroke/gradient
//! refusals). The layer plane covers fills, strokes and
//! image copies; every plane of a layer shares its placement.
//!
//! Stroke arrowheads (lane SMB-AI-ARROWHEADS-1, STU-VEC-146): a stroke row with a Folio
//! `StudioStrokeMarkers` entry becomes one [`FillPaint::StrokeGeometry`] row: the nib stroke outline
//! of the row's paths, trimmed by nib's `markers::marker_outlines` under `arrow_align.inside_path`,
//! united with the marker outlines (so dashes never dash a marker), painted with the row's paint,
//! blend mode and opacity. `NONE` / `arrow.none` at both ends is the plain stroke; the `ERD_*`
//! terminators are refused ([`REFUSE_MARKER_SHAPE_UNBUILT`]), as are markers on a brushed row, a
//! network stroke or a width-profiled path; other nib marker refusals carry nib's code. The
//! supersampled mode keeps its stroke refusal (no marker is drawn there). Shapes: nib's table,
//! UNVERIFIED (D5).
//!
//! Refusals are typed per layer and never dropped (`PreparedVectors::refusal`): unresolved paths or
//! images, gradients, strokes, colour profiles composite would have to convert, budgets. Hostile sizes
//! (V4-01/02): plane pixels and the sample work are checked before allocation.
#![deny(clippy::arithmetic_side_effects)]
use crate::CompositeError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_nib::PathLimits;
/// The path model of [`VectorGeometry`] (nib owns vector geometry, CX-CAP-001), re-exported so
/// folio-free callers (renderer tests) build specs without a nib edge of their own.
pub use hsk_studio_nib::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::render_plan::{AlphaConvention, SampleFormat};
use hsk_studio_pigment::transform_math::{Affine2, EdgeMode, LayerTransform, PreparedTransform};
use hsk_studio_pigment::{Mask, Rect};
use kurbo::{BezPath, PathEl, Shape};

/// Largest sample work of one layer: plane pixels x samples per pixel x path elements.
pub const MAX_VECTOR_WORK: u64 = 4_000_000_000;
/// Largest number of image copies one `TILE` row places.
pub const MAX_IMAGE_TILE_COPIES: u64 = 256;
/// The resampling filter and edge of image paint copies (pigment's layer transform).
pub const IMAGE_FILTER: hsk_studio_pigment::transform_math::ResampleFilter =
    hsk_studio_pigment::transform_math::ResampleFilter::Bilinear;
/// Deepest nesting of boolean operands.
pub const MAX_BOOLEAN_DEPTH: usize = 64;

pub const REFUSE_VECTOR_GEOMETRY: &str = "vector_geometry";
pub const REFUSE_VECTOR_OVER_LIMIT: &str = "vector_over_limit";
pub const REFUSE_VECTOR_WORK: &str = "vector_work_limit";
pub const REFUSE_IMAGE_TRANSFORM: &str = "image_paint_transform";
pub const REFUSE_IMAGE_TILE_COUNT: &str = "image_paint_tile_count";
pub const REFUSE_IMAGE_BOUNDS: &str = "image_paint_bounds";
pub const REFUSE_FILL: &str = "vector_fill";
/// A stroke row whose paint is an image or another stroke.
pub const REFUSE_STROKE_PAINT: &str = "vector_stroke_paint";
/// A fill row above a stroke row (strokes paint above fills).
pub const REFUSE_STROKE_ORDER: &str = "vector_stroke_order";
pub use crate::vector_nib::{PaintedTile, REFUSE_NIB_BUDGET, REFUSE_PAINT, REFUSE_STROKE_STYLE};
/// nib's paint and stroke models, re-exported for folio-free callers (renderer tests).
pub use hsk_studio_nib::raster::{
    Gradient, GradientGeometry, GradientStop, Interpolation as GradientInterpolation, PatternTile,
    Spread,
};
pub use hsk_studio_nib::stroke::{DashPattern, LineCap, LineJoin, StrokeAlign, StrokeStyle};

/// The four live boolean operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BooleanOp {
    Union,
    Intersect,
    Subtract,
    Exclude,
}

/// The shape of a vector layer in plan pixels.
#[derive(Clone, Debug, PartialEq)]
pub enum VectorGeometry {
    /// Paths united per sample (each with its own fill rule).
    Paths(Vec<OwnedPath>),
    /// A live boolean of operand shapes, bottom first.
    Boolean {
        op: BooleanOp,
        operands: Vec<VectorGeometry>,
    },
}

/// Image scaling mode (STU-VEC-139).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageScale {
    Fill,
    Fit,
    Crop,
    Tile,
}

/// What the caller states about a decoded image (like `TileInfo` for raster tiles).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub format: SampleFormat,
    pub alpha: AlphaConvention,
    pub colour_sha256: [u8; 32],
    pub alpha_sha256: [u8; 32],
}

/// An image fill row.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageFill {
    /// Caller-chosen id of the image bytes (Folio: the content digest); the lowering lists it
    /// against the plan's `SourceKey` so the caller can serve the decoded planes.
    pub source_id: String,
    pub info: ImageInfo,
    pub scale: ImageScale,
    /// `[a, b, c, d, e, f]`, Folio's unit-square convention.
    pub transform: [f64; 6],
    pub scaling_factor: f64,
    pub rotation: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FillPaint {
    /// Straight RGBA in the plan's blend space.
    Solid([f32; 4]),
    Image(ImageFill),
    /// A nib gradient in plan pixels (colours in plan units), evaluated over the layer plane.
    Gradient(Gradient),
    /// A nib raster pattern in plan pixels, evaluated over the layer plane.
    Pattern(PatternTile),
    /// A stroke row: `paint` (solid, gradient or pattern) inside the nib stroke outline of the
    /// layer's geometry instead of its fill. Stroke rows follow every fill row.
    Stroke {
        style: StrokeStyle,
        paint: Box<FillPaint>,
    },
    /// A stroke row with its own geometry (lane SMB-COMPOSITE-STROKE-GEOMETRY): `paint` (solid,
    /// gradient or pattern) inside `outline`, the filled outline of the stroke as the caller drew it
    /// (several paths are united, each under its own fill rule), instead of the nib stroke of the
    /// layer's fill geometry. Vector networks (stroke geometry differs from fill geometry) and
    /// variable-width strokes use it. Like [`FillPaint::Stroke`] it follows every fill row and has
    /// its own coverage plane ([`StrokePlane`]).
    StrokeGeometry {
        outline: Vec<OwnedPath>,
        paint: Box<FillPaint>,
    },
}

/// The stroke of a stroke row: a nib stroke style over the layer geometry, or an explicit outline.
#[derive(Clone, Copy)]
enum RowStroke<'a> {
    Style(&'a StrokeStyle),
    Outline(&'a [OwnedPath]),
}

/// The united filled outline of a [`FillPaint::StrokeGeometry`] row (`None` when empty).
fn explicit_outline(outline: &[OwnedPath], limits: &PathLimits) -> Result<Option<OwnedPath>, &'static str> {
    for p in outline {
        p.validate(limits).map_err(|_| REFUSE_VECTOR_GEOMETRY)?;
    }
    let drawn: Vec<OwnedPath> = outline.iter().filter(|p| !p.elements.is_empty()).cloned().collect();
    let united = match drawn.len() {
        0 => return Ok(None),
        1 => hsk_studio_nib::boolean::normalise_path(&drawn[0], &hsk_studio_nib::boolean::BooleanOptions::default()),
        _ => hsk_studio_nib::boolean::boolean(&drawn, hsk_studio_nib::boolean::BooleanOp::Union, &hsk_studio_nib::boolean::BooleanOptions::default()),
    };
    match united {
        Ok(g) => Ok(Some(g.path)),
        Err(hsk_studio_nib::Error::EmptyPathfinderResult) => Ok(None),
        Err(hsk_studio_nib::Error::BudgetExceeded) => Err(crate::vector_nib::REFUSE_NIB_BUDGET),
        Err(_) => Err(REFUSE_VECTOR_GEOMETRY),
    }
}

/// One fill row (bottom first): its paint, blend mode and opacity fraction.
#[derive(Clone, Debug, PartialEq)]
pub struct FillSpec {
    pub paint: FillPaint,
    pub mode: StudioBlendMode,
    pub opacity: f32,
}

/// A folio-free vector layer: what [`prepare_vector_layers`] draws.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorLayerSpec {
    pub layer_id: String,
    pub geometry: VectorGeometry,
    pub fills: Vec<FillSpec>,
}

/// How a layer's fill coverage is computed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VectorCoverage {
    /// Studio supersampling (module docs; the original rule, kept selectable).
    Supersampled,
    /// nib's exact signed-area coverage of the layer outline (`crate::vector_nib`); the default
    /// (lead decision, START_LOG: nib owns coverage, CX-CAP-001).
    #[default]
    Exact,
}

#[derive(Clone, Copy, Debug)]
pub struct VectorResolution {
    /// Samples per axis, `1..=16` (supersampled coverage).
    pub aa_samples: u8,
    /// Fill coverage method; strokes always use nib's exact coverage.
    pub coverage: VectorCoverage,
    /// Pixels of one layer plane, checked before it is allocated.
    pub max_layer_pixels: u64,
    pub path_limits: PathLimits,
}
impl Default for VectorResolution {
    fn default() -> Self {
        Self {
            aa_samples: 4,
            coverage: VectorCoverage::Exact,
            max_layer_pixels: 64 << 20,
            path_limits: PathLimits::default(),
        }
    }
}

/// A prepared fill row.
#[derive(Clone, Debug, PartialEq)]
pub enum PreparedPaint {
    Solid([f32; 4]),
    /// The image and the affine of every copy (image pixel space -> plan pixels).
    Image {
        source_id: String,
        info: ImageInfo,
        placements: Vec<Affine2>,
    },
    /// A gradient or pattern evaluated by nib over the layer plane (straight RGBA tile).
    Painted(PaintedTile),
}
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedFill {
    pub paint: PreparedPaint,
    pub mode: StudioBlendMode,
    pub opacity: f32,
}

/// One vector layer's coverage plane (`coverage_u8`, row-major, stride = width) and fills.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorPlane {
    pub layer_id: String,
    /// `None` when the shape is empty.
    pub placement: Option<Rect>,
    pub coverage: Vec<u8>,
    pub sha256: [u8; 32],
    /// Every row, bottom first (fills, then strokes).
    pub fills: Vec<PreparedFill>,
    /// The nib stroke coverage of every stroke row (same placement as the fill plane).
    pub strokes: Vec<StrokePlane>,
}

/// Largest clip plane in pixels (checked before nib allocates it).
pub const MAX_CLIP_PIXELS: u64 = 64 << 20;

/// A clipped or vector-masked layer's rasterised plane, row-major: `coverage_u8` (a clip alone,
/// stride = width) or `coverage_u16le` (`wide`: a vector mask, possibly combined with a clip,
/// stride = 2 x width; `crate::vector_mask`).
#[derive(Clone, Debug, PartialEq)]
pub struct ClipPlane {
    pub layer_id: String,
    pub placement: Rect,
    pub coverage: Vec<u8>,
    pub sha256: [u8; 32],
    pub wide: bool,
}
impl ClipPlane {
    pub fn mask(&self) -> Mask<'_> {
        let width = u64::from(self.placement.width);
        Mask {
            bounds: self.placement,
            sample_format: if self.wide { "coverage_u16le" } else { "coverage_u8" },
            bytes: &self.coverage,
            stride_bytes: if self.wide { width.saturating_mul(2) } else { width },
            sha256: self.sha256,
        }
    }
}

/// One stroke row's coverage (`coverage_u8` over the layer plane).
#[derive(Clone, Debug, PartialEq)]
pub struct StrokePlane {
    /// Index of the row in [`VectorPlane::fills`].
    pub row: usize,
    pub coverage: Vec<u8>,
    pub sha256: [u8; 32],
}

impl VectorPlane {
    /// The stroke plane of row `row` as pigment's mask.
    pub fn stroke_mask(&self, row: usize) -> Option<Mask<'_>> {
        let bounds = self.placement?;
        let s = self.strokes.iter().find(|s| s.row == row)?;
        Some(Mask {
            bounds,
            sample_format: "coverage_u8",
            bytes: &s.coverage,
            stride_bytes: u64::from(bounds.width),
            sha256: s.sha256,
        })
    }
    /// Is row `row` a stroke row?
    pub fn is_stroke(&self, row: usize) -> bool {
        self.strokes.iter().any(|s| s.row == row)
    }
    /// The plane as pigment's mask.
    pub fn mask(&self) -> Option<Mask<'_>> {
        let bounds = self.placement?;
        Some(Mask {
            bounds,
            sample_format: "coverage_u8",
            bytes: &self.coverage,
            stride_bytes: u64::from(bounds.width),
            sha256: self.sha256,
        })
    }
}

/// Prepared vector layers keyed by layer id, and the typed refusal of every layer that could not
/// be prepared (rendering it is `Unavailable` with that code).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreparedVectors {
    planes: Vec<VectorPlane>,
    refused: Vec<(String, &'static str)>,
    /// Clipped layer -> its clip outline (`None` = an empty clip region: nothing shows) or the
    /// refusal code of a clip that could not be resolved.
    clips: Vec<(String, Result<Option<OwnedPath>, &'static str>)>,
    /// Masked layer -> its vector mask (`crate::vector_mask`, Folio `StudioMaskAttachment::mask_id`)
    /// or the refusal code of a mask that could not be resolved.
    masks: Vec<(String, Result<std::sync::Arc<crate::vector_mask::VectorMask>, &'static str>)>,
    /// Masks whose source the caller's backend renders (`crate::vector_mask` module docs).
    pending: Vec<crate::vector_mask::PendingMaskSource>,
    /// Live path and area text bindings and what was drawn of each (`crate::vector_text`).
    texts: Vec<crate::vector_text::TextReport>,
}
impl PreparedVectors {
    /// Masks waiting for their source to be rendered by the caller's backend (raster, text or
    /// image-paint content): lower each with [`crate::mask_source_lowerings`], execute it, then
    /// [`Self::resolve_mask_source`] (or [`Self::fail_mask_source`]).
    pub fn pending_mask_sources(&self) -> &[crate::vector_mask::PendingMaskSource] {
        &self.pending
    }
    /// Folio-free: a mask whose source the caller renders; its layers are
    /// `vector_mask_source_not_rendered` until [`Self::resolve_mask_source`].
    pub fn with_pending_mask(mut self, pending: crate::vector_mask::PendingMaskSource) -> Self {
        for layer in &pending.layers {
            self.masks.retain(|(l, _)| l != layer);
            self.masks.push((
                layer.clone(),
                Err(crate::vector_mask::REFUSE_MASK_SOURCE_NOT_RENDERED),
            ));
        }
        self.pending.retain(|p| p.mask_id != pending.mask_id);
        self.pending.push(pending);
        self
    }
    /// The rendered source of pending mask `mask_id`: `frame` = straight RGBA of `rect`
    /// (row-major; `None` rect = empty content, empty frame), the source's own appearance already
    /// applied by its lowering. Its values become the mask content ([`crate::MaskContentMode::value`])
    /// and every masked layer gets the resolved mask. Typed refusals: unknown id
    /// (`vector_mask_unresolved`), a frame of the wrong size or over `MAX_CLIP_PIXELS`
    /// (`vector_mask_budget`), a non-finite or out-of-unit sample (`vector_mask_source_unavailable`);
    /// the pending entry stays on refusal.
    pub fn resolve_mask_source(
        &mut self,
        mask_id: &str,
        rect: Option<Rect>,
        frame: &[[f32; 4]],
    ) -> Result<(), &'static str> {
        use crate::vector_mask::{REFUSE_MASK_BUDGET, REFUSE_MASK_SOURCE, REFUSE_MASK_UNRESOLVED, VectorMaskSource};
        let at = self
            .pending
            .iter()
            .position(|p| p.mask_id == mask_id)
            .ok_or(REFUSE_MASK_UNRESOLVED)?;
        let pixels = match rect {
            Some(r) => u64::from(r.width)
                .checked_mul(u64::from(r.height))
                .filter(|p| *p <= MAX_CLIP_PIXELS)
                .ok_or(REFUSE_MASK_BUDGET)?,
            None => 0,
        };
        if u64::try_from(frame.len()).map_err(|_| REFUSE_MASK_BUDGET)? != pixels {
            return Err(REFUSE_MASK_BUDGET);
        }
        let p = &self.pending[at];
        let mut values = Vec::with_capacity(frame.len());
        for px in frame {
            if !px.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
                return Err(REFUSE_MASK_SOURCE);
            }
            values.push(p.mode.value(hsk_studio_pigment::blend_math::Rgba::new([px[0], px[1], px[2]], px[3])));
        }
        let p = self.pending.remove(at);
        let mut mask = p.template;
        mask.source = VectorMaskSource::Content { rect, values, outside: p.mode.outside() };
        let mask = std::sync::Arc::new(mask);
        for layer in &p.layers {
            self.masks.retain(|(l, _)| l != layer);
            self.masks.push((layer.clone(), Ok(mask.clone())));
        }
        Ok(())
    }
    /// A copy without the vector mask of `layer_id` (an unlinked mask leaves a composition layer's
    /// static subtree, `crate::composition::unlinked_mask_matte`).
    #[cfg(feature = "motion")]
    pub(crate) fn without_mask(&self, layer_id: &str) -> Self {
        let mut v = self.clone();
        v.masks.retain(|(l, _)| l != layer_id);
        v
    }
    /// The source of pending mask `mask_id` could not be rendered: its layers carry `code`.
    pub fn fail_mask_source(&mut self, mask_id: &str, code: &'static str) {
        let Some(at) = self.pending.iter().position(|p| p.mask_id == mask_id) else {
            return;
        };
        let p = self.pending.remove(at);
        for layer in &p.layers {
            self.masks.retain(|(l, _)| l != layer);
            self.masks.push((layer.clone(), Err(code)));
        }
    }
    /// The vector mask of `layer_id` (Folio `StudioMaskAttachment::mask_id`, or [`Self::with_mask`]).
    pub fn mask(&self, layer_id: &str) -> Option<Result<&crate::vector_mask::VectorMask, &'static str>> {
        self.masks
            .iter()
            .find(|(l, _)| l == layer_id)
            .map(|(_, m)| m.as_ref().map(|m| &**m).map_err(|e| *e))
    }
    /// Mask `layer_id` with `mask` (folio-free). A second mask on one layer replaces the first.
    pub fn with_mask(mut self, layer_id: &str, mask: crate::vector_mask::VectorMask) -> Self {
        self.masks.retain(|(l, _)| l != layer_id);
        self.masks.push((layer_id.to_owned(), Ok(std::sync::Arc::new(mask))));
        self
    }
    /// The clip of `layer_id` (Folio `StudioMaskAttachment::clip_id`, or [`Self::with_clip`]).
    pub fn clip(&self, layer_id: &str) -> Option<Result<Option<&OwnedPath>, &'static str>> {
        self.clips
            .iter()
            .find(|(l, _)| l == layer_id)
            .map(|(_, c)| c.as_ref().map(Option::as_ref).map_err(|e| *e))
    }
    /// Clip `layer_id` to the region of `outline` (folio-free; the outline's own fill rule). A
    /// second clip on one layer replaces the first.
    pub fn with_clip(mut self, layer_id: &str, outline: Option<OwnedPath>) -> Self {
        self.clips.retain(|(l, _)| l != layer_id);
        self.clips.push((layer_id.to_owned(), Ok(outline)));
        self
    }
    pub fn plane(&self, layer_id: &str) -> Option<&VectorPlane> {
        self.planes.iter().find(|p| p.layer_id == layer_id)
    }
    pub fn refusal(&self, layer_id: &str) -> Option<&'static str> {
        self.refused
            .iter()
            .find(|(l, _)| l == layer_id)
            .map(|(_, c)| *c)
    }
    pub fn planes(&self) -> &[VectorPlane] {
        &self.planes
    }
    /// The live-text report of story `text_id` (`crate::vector_text`).
    pub fn text(&self, text_id: &str) -> Option<&crate::vector_text::TextReport> {
        self.texts.iter().find(|t| t.text_id == text_id)
    }
    /// Every live-text report, in layer order.
    pub fn texts(&self) -> &[crate::vector_text::TextReport] {
        &self.texts
    }
    pub(crate) fn knows(&self, layer_id: &str) -> bool {
        self.plane(layer_id).is_some() || self.refusal(layer_id).is_some()
    }
    pub(crate) fn push_refusal(&mut self, layer_id: &str, code: &'static str) {
        self.refused.push((layer_id.to_owned(), code));
    }
    pub(crate) fn push_plane(&mut self, plane: VectorPlane) {
        self.planes.push(plane);
    }
}

/// A path ready for winding queries: explicit closes (fills close open subpaths), fill rule.
struct Closed {
    path: BezPath,
    even_odd: bool,
}

fn close(path: &OwnedPath, limits: &PathLimits) -> Result<Closed, &'static str> {
    path.validate(limits).map_err(|_| REFUSE_VECTOR_GEOMETRY)?;
    let source = path.to_kurbo().map_err(|_| REFUSE_VECTOR_GEOMETRY)?;
    let mut closed = BezPath::new();
    let mut open = false;
    for element in source.elements() {
        match element {
            PathEl::MoveTo(_) => {
                if open {
                    closed.close_path();
                }
                open = false;
                closed.push(*element);
            }
            PathEl::ClosePath => {
                closed.push(*element);
                open = false;
            }
            _ => {
                closed.push(*element);
                open = true;
            }
        }
    }
    if open {
        closed.close_path();
    }
    Ok(Closed {
        path: closed,
        even_odd: path.fill_rule == FillRule::EvenOdd,
    })
}

/// The geometry with every path closed, the bounding box (f64) and the element count.
enum Shape0 {
    Paths(Vec<Closed>),
    Boolean { op: BooleanOp, operands: Vec<Shape0> },
}

struct Built {
    shape: Shape0,
    bbox: Option<kurbo::Rect>,
    elements: u64,
}

fn union_box(a: Option<kurbo::Rect>, b: Option<kurbo::Rect>) -> Option<kurbo::Rect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.union(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

fn build(g: &VectorGeometry, limits: &PathLimits, depth: usize) -> Result<Built, &'static str> {
    if depth > MAX_BOOLEAN_DEPTH {
        return Err(REFUSE_VECTOR_GEOMETRY);
    }
    match g {
        VectorGeometry::Paths(paths) => {
            let mut closed = Vec::with_capacity(paths.len());
            let (mut bbox, mut elements) = (None, 0_u64);
            for p in paths {
                let c = close(p, limits)?;
                if !c.path.elements().is_empty() {
                    bbox = union_box(bbox, Some(c.path.bounding_box()));
                }
                elements = elements
                    .checked_add(u64::try_from(c.path.elements().len()).map_err(|_| REFUSE_VECTOR_WORK)?)
                    .ok_or(REFUSE_VECTOR_WORK)?;
                closed.push(c);
            }
            Ok(Built {
                shape: Shape0::Paths(closed),
                bbox,
                elements,
            })
        }
        VectorGeometry::Boolean { op, operands } => {
            if operands.is_empty() {
                return Err(REFUSE_VECTOR_GEOMETRY);
            }
            let next = depth.checked_add(1).ok_or(REFUSE_VECTOR_GEOMETRY)?;
            let mut shapes = Vec::with_capacity(operands.len());
            let (mut bbox, mut elements) = (None, 0_u64);
            for operand in operands {
                let b = build(operand, limits, next)?;
                bbox = union_box(bbox, b.bbox);
                elements = elements.checked_add(b.elements).ok_or(REFUSE_VECTOR_WORK)?;
                shapes.push(b.shape);
            }
            Ok(Built {
                shape: Shape0::Boolean {
                    op: *op,
                    operands: shapes,
                },
                bbox,
                elements,
            })
        }
    }
}

fn inside(shape: &Shape0, p: kurbo::Point) -> bool {
    match shape {
        Shape0::Paths(paths) => paths.iter().any(|c| {
            let w = c.path.winding(p);
            if c.even_odd { w & 1 != 0 } else { w != 0 }
        }),
        Shape0::Boolean { op, operands } => match op {
            BooleanOp::Union => operands.iter().any(|s| inside(s, p)),
            BooleanOp::Intersect => operands.iter().all(|s| inside(s, p)),
            BooleanOp::Subtract => {
                let mut it = operands.iter();
                it.next().is_some_and(|first| inside(first, p)) && !it.any(|s| inside(s, p))
            }
            BooleanOp::Exclude => !operands.iter().filter(|s| inside(s, p)).count().is_multiple_of(2),
        },
    }
}

/// The smallest rectangle holding both (`None` on overflow).
fn union_rect(a: Rect, b: Rect) -> Option<Rect> {
    let (ax, ay) = a.end().ok()?;
    let (bx, by) = b.end().ok()?;
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    Some(Rect {
        x,
        y,
        width: u32::try_from(ax.max(bx).checked_sub(x)?).ok()?,
        height: u32::try_from(ay.max(by).checked_sub(y)?).ok()?,
    })
}

/// Folio's `[a, b, c, d, e, f]` (`x' = a x + c y + e`, `y' = b x + d y + f`) as pigment's affine.
fn folio_affine(t: [f64; 6]) -> Affine2 {
    Affine2 {
        m: [t[0], t[2], t[4], t[1], t[3], t[5]],
    }
}
fn scale_translate(sx: f64, sy: f64, tx: f64, ty: f64) -> Affine2 {
    Affine2 {
        m: [sx, 0.0, tx, 0.0, sy, ty],
    }
}

/// Image pixel space -> plan pixels for every copy of an image row over the object's bounds.
fn placements(fill: &ImageFill, bounds: kurbo::Rect) -> Result<Vec<Affine2>, &'static str> {
    let (x0, y0, bw, bh) = (bounds.x0, bounds.y0, bounds.width(), bounds.height());
    let (w, h) = (f64::from(fill.info.width), f64::from(fill.info.height));
    if !(bw > 0.0 && bh > 0.0 && w > 0.0 && h > 0.0) {
        return Err(REFUSE_IMAGE_BOUNDS);
    }
    let identity = fill.transform == [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    if fill.scale != ImageScale::Crop && (!identity || fill.rotation != 0.0) {
        return Err(REFUSE_IMAGE_TRANSFORM);
    }
    let centred = |s: f64| scale_translate(s, s, x0 + (bw - w * s) / 2.0, y0 + (bh - h * s) / 2.0);
    Ok(match fill.scale {
        ImageScale::Crop => {
            if fill.rotation != 0.0 {
                return Err(REFUSE_IMAGE_TRANSFORM);
            }
            let to_unit = scale_translate(1.0 / w, 1.0 / h, 0.0, 0.0);
            let to_bounds = scale_translate(bw, bh, x0, y0);
            vec![to_unit.then(folio_affine(fill.transform)).then(to_bounds)]
        }
        ImageScale::Fill => vec![centred((bw / w).max(bh / h))],
        ImageScale::Fit => vec![centred((bw / w).min(bh / h))],
        ImageScale::Tile => {
            let s = fill.scaling_factor;
            if !(s.is_finite() && s > 0.0) {
                return Err(REFUSE_IMAGE_TRANSFORM);
            }
            let (cw, ch) = (w * s, h * s);
            let (nx, ny) = ((bw / cw).ceil(), (bh / ch).ceil());
            let limit = MAX_IMAGE_TILE_COPIES as f64;
            if !(nx.is_finite() && ny.is_finite()) || nx * ny > limit {
                return Err(REFUSE_IMAGE_TILE_COUNT);
            }
            let (nx, ny) = (nx as u64, ny as u64);
            let mut out = Vec::new();
            for j in 0..ny {
                for i in 0..nx {
                    out.push(scale_translate(s, s, x0 + i as f64 * cw, y0 + j as f64 * ch));
                }
            }
            out
        }
    })
}

/// Draws one spec: the coverage plane, the stroke planes and the prepared rows, or the refusal
/// code.
fn prepare_one(
    spec: &VectorLayerSpec,
    res: &VectorResolution,
    cancel: &CancellationToken,
) -> Result<Result<VectorPlane, &'static str>, CompositeError> {
    let built = match build(&spec.geometry, &res.path_limits, 0) {
        Ok(b) => b,
        Err(code) => return Ok(Err(code)),
    };
    // Rows: fills first, then strokes (strokes paint above fills, Figma order; a fill row above a
    // stroke row is refused, never reordered silently).
    let mut seen_stroke = false;
    let mut rows: Vec<(&FillPaint, Option<RowStroke<'_>>, &FillSpec)> =
        Vec::with_capacity(spec.fills.len());
    for f in &spec.fills {
        if !(f.opacity.is_finite() && (0.0..=1.0).contains(&f.opacity)) {
            return Ok(Err(REFUSE_FILL));
        }
        match &f.paint {
            FillPaint::Stroke { style, paint } => {
                if matches!(**paint, FillPaint::Stroke { .. } | FillPaint::StrokeGeometry { .. } | FillPaint::Image(_)) {
                    return Ok(Err(REFUSE_STROKE_PAINT));
                }
                seen_stroke = true;
                rows.push((paint, Some(RowStroke::Style(style)), f));
            }
            FillPaint::StrokeGeometry { outline, paint } => {
                if matches!(**paint, FillPaint::Stroke { .. } | FillPaint::StrokeGeometry { .. } | FillPaint::Image(_)) {
                    return Ok(Err(REFUSE_STROKE_PAINT));
                }
                seen_stroke = true;
                rows.push((paint, Some(RowStroke::Outline(outline)), f));
            }
            other => {
                if seen_stroke {
                    return Ok(Err(REFUSE_STROKE_ORDER));
                }
                rows.push((other, None, f));
            }
        }
    }
    // Stroke outlines (nib) and their bounds join the plane rect.
    let mut stroke_paths = Vec::new();
    let mut bbox = built.bbox;
    for (_, stroke, _) in &rows {
        let Some(stroke) = stroke else { continue };
        let path = match stroke {
            RowStroke::Style(style) => crate::vector_nib::stroke_outline(&spec.geometry, style),
            RowStroke::Outline(outline) => explicit_outline(outline, &res.path_limits),
        };
        let path = match path {
            Ok(p) => p,
            Err(code) => return Ok(Err(code)),
        };
        if let Some([x0, y0, x1, y1]) = path.as_ref().and_then(crate::vector_nib::path_bounds) {
            bbox = union_box(bbox, Some(kurbo::Rect::new(x0, y0, x1, y1)));
        }
        stroke_paths.push(path);
    }
    let mut image_placements: Vec<Option<Vec<Affine2>>> = Vec::with_capacity(rows.len());
    for (paint, _, _) in &rows {
        image_placements.push(match paint {
            FillPaint::Image(image) => {
                let Some(fill_box) = built.bbox else {
                    return Ok(Err(REFUSE_IMAGE_BOUNDS));
                };
                match placements(image, fill_box) {
                    Ok(p) => Some(p),
                    Err(code) => return Ok(Err(code)),
                }
            }
            FillPaint::Solid(rgba) => {
                if !rgba.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c)) {
                    return Ok(Err(REFUSE_FILL));
                }
                None
            }
            _ => None,
        });
    }
    let empty = || VectorPlane {
        layer_id: spec.layer_id.clone(),
        placement: None,
        coverage: Vec::new(),
        sha256: hsk_studio_prism::profile_hash(&[]),
        fills: Vec::new(),
        strokes: Vec::new(),
    };
    let Some(bbox) = bbox else {
        return Ok(Ok(empty()));
    };
    let (x0, y0) = (bbox.x0.floor(), bbox.y0.floor());
    let (x1, y1) = (bbox.x1.ceil(), bbox.y1.ceil());
    let side = |a: f64, b: f64| -> Option<u32> {
        let d = b - a;
        (d.is_finite() && d >= 0.0 && d <= f64::from(u32::MAX)).then_some(d as u32)
    };
    let (Some(width), Some(height)) = (side(x0, x1), side(y0, y1)) else {
        return Ok(Err(REFUSE_VECTOR_OVER_LIMIT));
    };
    if !(x0.abs() < 9.0e15 && y0.abs() < 9.0e15) {
        return Ok(Err(REFUSE_VECTOR_OVER_LIMIT));
    }
    if width == 0 || height == 0 {
        return Ok(Ok(empty()));
    }
    // The plane also covers every placed image copy (zero coverage outside the shape), so the
    // backends' mask-gap rule holds for the rows group (an image may reach beyond the shape: FILL).
    let mut plane_rect = Rect {
        x: x0 as i64,
        y: y0 as i64,
        width,
        height,
    };
    for ((paint, _, _), placed) in rows.iter().zip(&image_placements) {
        let (FillPaint::Image(image), Some(placed)) = (paint, placed) else {
            continue;
        };
        for a in placed {
            let t = LayerTransform::new(*a, IMAGE_FILTER).with_edge(EdgeMode::Clamp);
            let source = Rect {
                x: 0,
                y: 0,
                width: image.info.width,
                height: image.info.height,
            };
            let Ok(prepared) = PreparedTransform::new(&t, source) else {
                return Ok(Err(REFUSE_IMAGE_TRANSFORM));
            };
            let Some(united) = union_rect(plane_rect, prepared.bounds()) else {
                return Ok(Err(REFUSE_VECTOR_OVER_LIMIT));
            };
            plane_rect = united;
        }
    }
    let Some(pixels) = u64::from(plane_rect.width)
        .checked_mul(u64::from(plane_rect.height))
        .filter(|p| *p <= res.max_layer_pixels)
    else {
        return Ok(Err(REFUSE_VECTOR_OVER_LIMIT));
    };
    let coverage = match res.coverage {
        VectorCoverage::Supersampled => {
            match supersampled(&built, plane_rect, pixels, res, cancel)? {
                Ok(c) => c,
                Err(code) => return Ok(Err(code)),
            }
        }
        VectorCoverage::Exact => {
            let path = match crate::vector_nib::outline(&spec.geometry, 0) {
                Ok(p) => p,
                Err(code) => return Ok(Err(code)),
            };
            match crate::vector_nib::coverage(path.as_ref(), plane_rect, res.max_layer_pixels) {
                Ok(c) => c,
                Err(code) => return Ok(Err(code)),
            }
        }
    };
    cancel.check().map_err(|_| CompositeError::Canceled)?;
    let mut fills = Vec::with_capacity(rows.len());
    let mut strokes = Vec::new();
    let mut stroke_iter = stroke_paths.iter();
    for (index, ((paint, style, f), placed)) in rows.iter().zip(image_placements).enumerate() {
        let prepared = match (paint, placed) {
            (FillPaint::Solid(rgba), _) => PreparedPaint::Solid(*rgba),
            (FillPaint::Image(image), Some(placements)) => PreparedPaint::Image {
                source_id: image.source_id.clone(),
                info: image.info,
                placements,
            },
            (FillPaint::Gradient(g), _) => {
                let paint = hsk_studio_nib::raster::Paint::Gradient(g.clone());
                match crate::vector_nib::painted(&paint, plane_rect, res.max_layer_pixels) {
                    Ok(t) => PreparedPaint::Painted(t),
                    Err(code) => return Ok(Err(code)),
                }
            }
            (FillPaint::Pattern(t), _) => {
                let paint = hsk_studio_nib::raster::Paint::Pattern(t.clone());
                match crate::vector_nib::painted(&paint, plane_rect, res.max_layer_pixels) {
                    Ok(t) => PreparedPaint::Painted(t),
                    Err(code) => return Ok(Err(code)),
                }
            }
            _ => return Ok(Err(REFUSE_FILL)),
        };
        if style.is_some() {
            let path = stroke_iter.next().ok_or(CompositeError::Overflow)?;
            let c = match crate::vector_nib::coverage(path.as_ref(), plane_rect, res.max_layer_pixels) {
                Ok(c) => c,
                Err(code) => return Ok(Err(code)),
            };
            strokes.push(StrokePlane {
                row: index,
                sha256: hsk_studio_prism::profile_hash(&c),
                coverage: c,
            });
        }
        fills.push(PreparedFill {
            paint: prepared,
            mode: f.mode,
            opacity: f.opacity,
        });
    }
    Ok(Ok(VectorPlane {
        layer_id: spec.layer_id.clone(),
        placement: Some(plane_rect),
        sha256: hsk_studio_prism::profile_hash(&coverage),
        coverage,
        fills,
        strokes,
    }))
}

/// The Studio supersampled coverage of the module docs over `rect`.
fn supersampled(
    built: &Built,
    rect: Rect,
    pixels: u64,
    res: &VectorResolution,
    cancel: &CancellationToken,
) -> Result<Result<Vec<u8>, &'static str>, CompositeError> {
    if !(1..=16).contains(&res.aa_samples) {
        return Err(CompositeError::UnsupportedDescriptor("vector_aa_samples".into()));
    }
    let n = u64::from(res.aa_samples);
    let samples = n.checked_mul(n).ok_or(CompositeError::Overflow)?;
    let work = pixels
        .checked_mul(samples)
        .and_then(|w| w.checked_mul(built.elements.max(1)));
    if work.is_none_or(|w| w > MAX_VECTOR_WORK) {
        return Ok(Err(REFUSE_VECTOR_WORK));
    }
    let count = usize::try_from(pixels).map_err(|_| CompositeError::Overflow)?;
    let mut coverage = Vec::new();
    if coverage.try_reserve_exact(count).is_err() {
        return Ok(Err(REFUSE_VECTOR_OVER_LIMIT));
    }
    let (x0, y0) = (rect.x as f64, rect.y as f64);
    let step = 1.0 / f64::from(res.aa_samples);
    let half = samples.checked_div(2).ok_or(CompositeError::Overflow)?;
    for py in 0..rect.height {
        cancel.check().map_err(|_| CompositeError::Canceled)?;
        for px in 0..rect.width {
            let mut hits = 0_u64;
            for j in 0..res.aa_samples {
                for i in 0..res.aa_samples {
                    let p = kurbo::Point::new(
                        x0 + f64::from(px) + (f64::from(i) + 0.5) * step,
                        y0 + f64::from(py) + (f64::from(j) + 0.5) * step,
                    );
                    if inside(&built.shape, p) {
                        hits = hits.checked_add(1).ok_or(CompositeError::Overflow)?;
                    }
                }
            }
            // round(255 * hits / samples), integer.
            let v = hits
                .checked_mul(255)
                .and_then(|v| v.checked_add(half))
                .and_then(|v| v.checked_div(samples))
                .ok_or(CompositeError::Overflow)?;
            coverage.push(u8::try_from(v).map_err(|_| CompositeError::Overflow)?);
        }
    }
    Ok(Ok(coverage))
}

/// Draws every spec (folio-free; the Folio adapter is [`crate::prepare_vectors`]). A spec that
/// cannot be drawn is recorded with its refusal code; only cancellation and an invalid resolution
/// fail the call. Duplicate layer ids are `UnsupportedDescriptor("duplicate_vector_layer")`.
pub fn prepare_vector_layers(
    specs: &[VectorLayerSpec],
    res: &VectorResolution,
    cancel: &CancellationToken,
) -> Result<PreparedVectors, CompositeError> {
    let mut out = PreparedVectors::default();
    for spec in specs {
        cancel.check().map_err(|_| CompositeError::Canceled)?;
        if out.knows(&spec.layer_id) {
            return Err(CompositeError::UnsupportedDescriptor(
                "duplicate_vector_layer".into(),
            ));
        }
        match prepare_one(spec, res, cancel)? {
            Ok(plane) => out.push_plane(plane),
            Err(code) => out.push_refusal(&spec.layer_id, code),
        }
    }
    Ok(out)
}

// ---- Folio adapter ------------------------------------------------------------------------------

pub const REFUSE_PATH_UNRESOLVED: &str = "vector_path_unresolved";
pub const REFUSE_IMAGE_UNRESOLVED: &str = "image_paint_unresolved";
pub const REFUSE_PAINT_GRADIENT: &str = "paint_gradient";
pub const REFUSE_STROKE: &str = "vector_stroke_not_rendered";
pub const REFUSE_FILL_STYLE: &str = "vector_fill_style_unresolved";
pub const REFUSE_PAINT_PROFILE: &str = "paint_colour_profile";
pub const REFUSE_PAINT_LINEAR: &str = "paint_colour_linear_light";
pub const REFUSE_PAINT_MODE: &str = "paint_blend_mode";
pub const REFUSE_BOOLEAN_OPERAND: &str = "boolean_operand_kind";
/// A clip attachment naming a clip the vector records do not hold.
pub const REFUSE_CLIP_UNRESOLVED: &str = "vector_clip_unresolved";
/// A clipped layer that also has its own coverage mask or is clipped to a base (one mask slot).
pub const REFUSE_CLIP_WITH_MASK: &str = "vector_clip_with_mask";
/// A stroke weight, dash or offset, or a gradient, in a document whose geometry unit is not px
/// (composite has no document-unit -> plan-pixel scale; paths arrive in plan pixels from the caller).
pub const REFUSE_GEOMETRY_UNIT: &str = "vector_geometry_unit";
/// A row bound to a Folio pattern (STU-VEC-138): composite receives no pattern tile artwork.
pub const REFUSE_PAINT_PATTERN: &str = "paint_pattern_not_resolved";
/// A Folio vector network record nib refuses (indices, budgets, open loops, non-finite points).
pub const REFUSE_NETWORK: &str = "vector_network";
/// A network region style that holds a stroke row (a region style paints fills only).
pub const REFUSE_NETWORK_REGION_STYLE: &str = "vector_network_region_style";
/// A Folio width profile nib refuses (point count, order, range).
pub const REFUSE_WIDTH_PROFILE: &str = "vector_width_profile";
/// A width profile with independent outgoing widths (nib profiles are symmetric).
pub const REFUSE_WIDTH_ASYMMETRIC: &str = "vector_width_asymmetric";
/// A width-profiled stroke row with a dash, a projecting cap or a non-centre alignment.
pub const REFUSE_WIDTH_STYLE: &str = "vector_width_style";
/// An envelope nib refuses (geometry, mesh, top object) or a non-px document.
pub const REFUSE_ENVELOPE: &str = "vector_envelope";
/// An envelope asking to distort gradient or pattern fills (composite does not distort paints).
pub const REFUSE_ENVELOPE_PAINT: &str = "vector_envelope_paint";
/// A brush binding naming no brush, or a brush nib refuses.
pub const REFUSE_BRUSH: &str = "vector_brush";
/// A brush kind composite does not draw (every STU-VEC-162 kind is drawn since lane
/// SMB-ILLUSTRATOR-INTEROP; kept for forward compatibility).
pub const REFUSE_BRUSH_KIND: &str = "vector_brush_kind";
/// Colorization tints and shades or hue shift, or tints over a non-solid stroke paint.
pub const REFUSE_BRUSH_COLORIZATION: &str = "vector_brush_colorization";
/// A stroke marker token with no built shape (the six `ERD_*` connector terminators of
/// STU-VEC-146); no substitute is drawn. Other marker refusals carry nib's
/// `hsk_studio_nib::markers::MarkerError::code`.
pub const REFUSE_MARKER_SHAPE_UNBUILT: &str = "marker_shape_unbuilt";
/// Stroke markers on a brushed stroke row (no defined marker over brush art).
pub const REFUSE_MARKER_BRUSH: &str = "vector_marker_brush";
/// Stroke markers on a vector-network stroke (network endpoint markers, STU-VEC-062, not built).
pub const REFUSE_MARKER_NETWORK: &str = "vector_marker_network";
/// Stroke markers on a width-profiled path (the marker size follows a uniform weight).
pub const REFUSE_MARKER_WIDTH_PROFILE: &str = "vector_marker_width_profile";

/// Caller-owned resolution of what Folio references by id: `SVPT` path geometry (in plan pixels:
/// the caller applies the document unit and resolution) and decoded image paint bytes.
pub trait VectorAssets {
    fn path(&self, path_id: &str) -> Option<OwnedPath>;
    /// Info of the decoded image of `source` (Folio `StudioImageSource`, bound by sha256).
    fn image(&self, source: &hsk_studio_folio::StudioImageSource) -> Option<ImageInfo>;
    /// The Folio `StudioVectorNetwork` record of `path_id` when that path is a vector network
    /// (STU-VEC-108; vertices in plan pixels like [`VectorAssets::path`]). A network path draws its
    /// derived fill geometry and its stroke rows outline its derived stroke geometry (lane
    /// SMB-COMPOSITE-NETWORK-ADAPTER). Default: no networks.
    fn network(&self, _path_id: &str) -> Option<hsk_studio_folio::StudioVectorNetwork> {
        None
    }
    /// Plan colour of a Folio colour whose encoding the caller resolves (it knows the document
    /// working space; lane SMB-VECTOR-STYLES): `Some([r, g, b])` in plan units for `space`. Called
    /// after the profile check (a colour bound to another profile is still refused). Default
    /// `None`: composite's own rule (no conversion; a linear-light plan takes only 0 and 1).
    fn plan_colour(
        &self,
        _colour: &hsk_studio_folio::StudioColor,
        _space: hsk_studio_pigment::blend_math::BlendSpace,
    ) -> Option<[f32; 3]> {
        None
    }
    /// The story `text_id` of a Folio path- or area-text binding, shaped by the caller (lane
    /// SMB-AI-AREA-TYPE-1, `crate::vector_text`). Default `None`: the host draws no live text; the
    /// layer draws without it and the binding is reported (`vector_text_not_provided`).
    fn shaped_story(&self, _text_id: &str) -> Option<crate::vector_text::ShapedStory> {
        None
    }
}

/// The nib network of a Folio `StudioVectorNetwork` record, validated by nib (STU-VEC-108). Sizes are
/// checked against nib's network budgets before anything is allocated.
pub fn nib_network(
    n: &hsk_studio_folio::StudioVectorNetwork,
) -> Result<hsk_studio_nib::network::VectorNetwork, hsk_studio_nib::network::NetworkError> {
    use hsk_studio_folio::{
        StudioHandleMirroring as HM, StudioRegionWinding as W, StudioStrokeCap as C,
        StudioStrokeJoin as J,
    };
    use hsk_studio_nib::Mirroring;
    use hsk_studio_nib::network::{
        MAX_LOOP_REFERENCES, MAX_NETWORK_ELEMENTS, MAX_REGIONS, NetworkError, NetworkRegion, NetworkSegment,
        NetworkVertex, VectorNetwork,
    };
    use hsk_studio_nib::stroke::{LineCap, LineJoin};
    let references = n
        .regions
        .iter()
        .flat_map(|r| &r.loops)
        .try_fold(0_usize, |sum, l| sum.checked_add(l.len()));
    if n.vertices.len() > MAX_NETWORK_ELEMENTS
        || n.segments.len() > MAX_NETWORK_ELEMENTS
        || n.regions.len() > MAX_REGIONS
        || references.is_none_or(|r| r > MAX_LOOP_REFERENCES)
    {
        return Err(NetworkError::BudgetExceeded);
    }
    let idx = |v: u32| usize::try_from(v).map_err(|_| NetworkError::IndexOutOfBounds);
    let net = VectorNetwork {
        vertices: n
            .vertices
            .iter()
            .map(|v| NetworkVertex {
                x: v.x,
                y: v.y,
                stroke_cap: v.stroke_cap.map(|c| match c {
                    C::Butt => LineCap::Butt,
                    C::Round => LineCap::Round,
                    C::Projecting => LineCap::Square,
                }),
                stroke_join: v.stroke_join.map(|j| match j {
                    J::Miter => LineJoin::Miter,
                    J::Round => LineJoin::Round,
                    J::Bevel => LineJoin::Bevel,
                }),
                corner_radius: v.corner_radius,
                handle_mirroring: v.handle_mirroring.map(|m| match m {
                    HM::None => Mirroring::None,
                    HM::Angle => Mirroring::Angle,
                    HM::AngleAndLength => Mirroring::AngleAndLength,
                }),
            })
            .collect(),
        segments: n
            .segments
            .iter()
            .map(|s| {
                Ok(NetworkSegment {
                    start: idx(s.start)?,
                    end: idx(s.end)?,
                    tangent_start: s.tangent_start.map(|t| PathPoint::new(t[0], t[1])),
                    tangent_end: s.tangent_end.map(|t| PathPoint::new(t[0], t[1])),
                })
            })
            .collect::<Result<_, NetworkError>>()?,
        regions: n
            .regions
            .iter()
            .map(|r| {
                Ok(NetworkRegion {
                    winding_rule: match r.winding_rule {
                        W::NonZero => FillRule::NonZero,
                        W::EvenOdd => FillRule::EvenOdd,
                    },
                    loops: r
                        .loops
                        .iter()
                        .map(|l| l.iter().map(|k| idx(*k)).collect::<Result<Vec<_>, _>>())
                        .collect::<Result<_, _>>()?,
                    fills: r.fills,
                    fill_style_id: r.fill_style_id.clone(),
                })
            })
            .collect::<Result<_, NetworkError>>()?,
    };
    net.validate()?;
    Ok(net)
}

/// The id the lowering lists an image under: its content digest (`<algorithm>:<hex>`).
pub fn image_source_id(source: &hsk_studio_folio::StudioImageSource) -> String {
    use hsk_studio_folio::StudioImageSource;
    let d = match source {
        StudioImageSource::PackageAsset { content_digest }
        | StudioImageSource::ImportOriginal { content_digest, .. } => content_digest,
    };
    format!("{}:{}", d.algorithm, d.digest)
}

/// One brush laid along one stroke path: an outline per brush art path.
/// A nib swept along one stroke path (calligraphic, bristle).
type Sweep<'a> = Box<dyn Fn(&OwnedPath) -> Result<OwnedPath, hsk_studio_nib::Error> + 'a>;
type BrushDraw<'a> = Box<dyn Fn(&OwnedPath) -> Result<Vec<OwnedPath>, hsk_studio_nib::Error> + 'a>;

/// `arrow.none` / `NONE` (STU-VEC-146: no marker).
fn is_no_marker(token: &str) -> bool {
    hsk_studio_folio::StudioArrowhead::from_token(token).is_some_and(|a| a.is_none())
}

struct FolioCx<'a> {
    layers: std::collections::BTreeMap<&'a str, &'a hsk_studio_folio::StudioLayer>,
    /// Folio layer order (bottom first within a parent).
    order: Vec<&'a hsk_studio_folio::StudioLayer>,
    design: &'a hsk_studio_folio::StudioDesignRecords,
    records: &'a hsk_studio_folio::StudioVectorRecords,
    assets: &'a dyn VectorAssets,
    blending: &'a hsk_studio_folio::ProfileBinding,
    space: hsk_studio_pigment::blend_math::BlendSpace,
    /// The document's geometry unit is px (plan pixels; strokes and gradients need it).
    unit_px: bool,
    /// nib mode ([`VectorCoverage::Exact`]): strokes, gradients and pattern bindings are mapped
    /// (strokes and gradients rendered, pattern bindings refused). The default supersampled mode
    /// keeps the earlier Folio behaviour (`vector_stroke_not_rendered`, `paint_gradient`) so pinned
    /// consumers of the default are unchanged.
    nib: bool,
}

impl FolioCx<'_> {
    fn paths(&self, ids: &[String]) -> Result<Vec<OwnedPath>, &'static str> {
        ids.iter()
            .map(|id| self.assets.path(id).ok_or(REFUSE_PATH_UNRESOLVED))
            .collect()
    }

    /// The shape of a vector layer (its paths) or of a boolean group (its cached outline, else
    /// its operands combined).
    fn geometry(&self, layer_id: &str, depth: usize) -> Result<VectorGeometry, &'static str> {
        if depth > MAX_BOOLEAN_DEPTH {
            return Err(REFUSE_VECTOR_GEOMETRY);
        }
        let layer = self.layers.get(layer_id).ok_or(REFUSE_BOOLEAN_OPERAND)?;
        if let hsk_studio_folio::Payload::Vector { path_ids } = &layer.payload {
            return Ok(VectorGeometry::Paths(self.paths(path_ids)?));
        }
        let group = self
            .records
            .boolean_group(layer_id)
            .ok_or(REFUSE_BOOLEAN_OPERAND)?;
        if !group.fill_geometry_path_ids.is_empty() {
            return Ok(VectorGeometry::Paths(
                self.paths(&group.fill_geometry_path_ids)?,
            ));
        }
        let next = depth.checked_add(1).ok_or(REFUSE_VECTOR_GEOMETRY)?;
        let operands = group
            .operands
            .iter()
            .map(|o| self.geometry(o, next))
            .collect::<Result<Vec<_>, _>>()?;
        use hsk_studio_folio::StudioBooleanOperation as Op;
        Ok(VectorGeometry::Boolean {
            op: match group.operation {
                Op::Union => BooleanOp::Union,
                Op::Intersect => BooleanOp::Intersect,
                Op::Subtract => BooleanOp::Subtract,
                Op::Exclude => BooleanOp::Exclude,
            },
            operands,
        })
    }

    /// A paint colour in plan units (no conversion: another profile is refused, and a linear-light
    /// plan takes only black and white, which are the same in every encoding).
    fn colour(&self, c: &hsk_studio_folio::StudioColor) -> Result<[f32; 4], &'static str> {
        use hsk_studio_folio::ProfileBinding;
        if let ProfileBinding::Assigned(p) = &c.colour_profile_id
            && !matches!(self.blending, ProfileBinding::Assigned(b) if b == p)
        {
            return Err(REFUSE_PAINT_PROFILE);
        }
        if let Some(rgb) = self.assets.plan_colour(c, self.space) {
            if rgb.iter().any(|v| !v.is_finite()) {
                return Err(REFUSE_PAINT);
            }
            return Ok([rgb[0], rgb[1], rgb[2], 1.0]);
        }
        let rgb = [c.r as f32, c.g as f32, c.b as f32];
        if self.space == hsk_studio_pigment::blend_math::BlendSpace::LinearLight
            && rgb.iter().any(|v| *v != 0.0 && *v != 1.0)
        {
            return Err(REFUSE_PAINT_LINEAR);
        }
        Ok([rgb[0], rgb[1], rgb[2], 1.0])
    }

    /// A Folio paint as a row paint (solid in plan units, or a nib gradient).
    fn paint(&self, paint: &hsk_studio_folio::StudioPaint) -> Result<FillPaint, &'static str> {
        use hsk_studio_folio::{GradientInterpolation as I, GradientSpread as Sp, StudioGradientGeometry as G, StudioPaint};
        match paint {
            StudioPaint::Solid { colour } => Ok(FillPaint::Solid(self.colour(colour)?)),
            StudioPaint::Gradient { gradient } => {
                if !self.nib {
                    return Err(REFUSE_PAINT_GRADIENT);
                }
                if !self.unit_px {
                    return Err(REFUSE_GEOMETRY_UNIT);
                }
                let mut stops = Vec::with_capacity(gradient.stops.len());
                for st in &gradient.stops {
                    let [r, g, b, _] = self.colour(&st.colour)?;
                    stops.push(GradientStop {
                        offset: st.position / 100.0,
                        colour: [r, g, b, (st.opacity / 100.0) as f32],
                        midpoint: st.midpoint.map(|m| m / 100.0),
                    });
                }
                Ok(FillPaint::Gradient(Gradient {
                    geometry: match gradient.geometry {
                        G::Linear { x1, y1, x2, y2 } => GradientGeometry::Linear { x1, y1, x2, y2 },
                        G::Radial { cx, cy, r, fx, fy, fr } => {
                            GradientGeometry::Radial { cx, cy, r, fx, fy, fr }
                        }
                    },
                    transform: gradient.transform,
                    spread: match gradient.spread {
                        Sp::Pad => Spread::Pad,
                        Sp::Reflect => Spread::Reflect,
                        Sp::Repeat => Spread::Repeat,
                    },
                    interpolation: match gradient.interpolation {
                        I::Classic => GradientInterpolation::Classic,
                        I::Perceptual => GradientInterpolation::Perceptual,
                    },
                    dither: gradient.dither,
                    stops,
                }))
            }
        }
    }

    /// A Folio stroke style as nib's (px document only).
    fn stroke_style(&self, s: &hsk_studio_folio::StudioStrokeStyle) -> Result<StrokeStyle, &'static str> {
        use hsk_studio_folio::{StrokeAlign as A, StrokeCap as C, StrokeJoin as J};
        let px = |l: &hsk_studio_folio::Length| -> Result<f64, &'static str> {
            if l.unit == hsk_studio_folio::GeometryUnit::Px && self.unit_px {
                Ok(l.value)
            } else {
                Err(REFUSE_GEOMETRY_UNIT)
            }
        };
        let dash = if s.dash.is_empty() {
            None
        } else {
            Some(DashPattern {
                lengths: s.dash.iter().map(px).collect::<Result<Vec<_>, _>>()?,
                phase: px(&s.dash_offset)?,
            })
        };
        Ok(StrokeStyle {
            width: px(&s.weight)?,
            cap: match s.cap {
                C::Butt => LineCap::Butt,
                C::Round => LineCap::Round,
                C::Projecting => LineCap::Square,
            },
            join: match s.join {
                J::Miter => LineJoin::Miter,
                J::Round => LineJoin::Round,
                J::Bevel => LineJoin::Bevel,
            },
            miter_limit: s.miter_limit,
            dash,
            align: match s.align {
                A::Center => StrokeAlign::Center,
                A::Inside => StrokeAlign::Inside,
                A::Outside => StrokeAlign::Outside,
            },
        })
    }

    /// Own fill and stroke geometry of a vector layer (lanes SMB-COMPOSITE-NETWORK-ADAPTER,
    /// SMB-VECTOR-STYLES and SMB-ILLUSTRATOR-TOOLS-2; STU-VEC-108, STU-VEC-122, STU-VEC-026,
    /// STU-VEC-055/168/169). A network path ([`VectorAssets::network`]) fills its filled regions
    /// without a `fill_style_id` and strokes its derived `stroke_geometry` (every segment once); a
    /// filled region with its own `fill_style_id` is painted by that style's fill rows, each as a
    /// [`FillPaint::StrokeGeometry`] row over the region outline, placed after the layer's fill rows
    /// and before its stroke rows (a region style with a stroke row is refused). A path with a Folio
    /// width profile (`StudioVectorRecords::width_profile`) strokes with nib's variable-width
    /// outline. A stroke row bound to a brush (`StudioBrushStroke`) becomes one
    /// [`FillPaint::StrokeGeometry`] row per brush art path: nib's art, scatter or pattern brush
    /// along the stroke paths, painted with the art colour (`colorize.none`) or its tint of a solid
    /// stroke colour (`colorize.tints`: `c' = 1 - (1 - L) (1 - stroke)`, L the art colour's Rec. 709
    /// luminance in plan units; Studio rule). Every other stroke row becomes a
    /// [`FillPaint::StrokeGeometry`] row over the union of the stroke outlines. An envelope
    /// (`StudioEnvelope`) wraps the path geometry before the appearance is drawn, or with
    /// `distort_appearance` the drawn fill and stroke geometry, through nib's `envelope_paths`
    /// (one frame: the union bounds of everything it wraps); paints are not distorted. Plain paths
    /// fill and stroke themselves; a layer with none of these is returned unchanged.
    fn own_geometry(
        &self,
        layer_id: &str,
        payload: &hsk_studio_folio::Payload,
        geometry: VectorGeometry,
        fills: Vec<FillSpec>,
    ) -> Result<(VectorGeometry, Vec<FillSpec>), &'static str> {
        let hsk_studio_folio::Payload::Vector { path_ids } = payload else {
            return Ok((geometry, fills));
        };
        let networks: Vec<_> = path_ids.iter().map(|id| self.assets.network(id)).collect();
        let profiles: Vec<_> = path_ids.iter().map(|id| self.records.width_profile(id)).collect();
        let envelope = self.records.envelope(layer_id);
        let style_id = self.design.node_design(layer_id).and_then(|n| n.style_refs.get("fill_style_id"));
        let rows_of = style_id.map(|id| self.visible_rows(id)).unwrap_or_default();
        let brushed = |k: usize| -> Option<&hsk_studio_folio::StudioBrushStroke> {
            self.records.brush_stroke(style_id?, rows_of.get(k).copied()?)
        };
        let any_brush = (0..fills.len()).any(|k| brushed(k).is_some());
        // Stroke arrowheads of a row; an entry with no marker at either end is the plain stroke.
        let marked = |k: usize| -> Option<&hsk_studio_folio::StudioStrokeMarkers> {
            self.records
                .stroke_marker(style_id?, rows_of.get(k).copied()?)
                .filter(|m| !(is_no_marker(&m.start) && is_no_marker(&m.end)))
        };
        let any_marker = (0..fills.len()).any(|k| marked(k).is_some());
        let has_network = networks.iter().any(Option::is_some);
        if !has_network && profiles.iter().all(Option::is_none) && envelope.is_none() && !any_brush && !any_marker {
            return Ok((geometry, fills));
        }
        let VectorGeometry::Paths(paths) = geometry else {
            return Err(REFUSE_VECTOR_GEOMETRY);
        };
        let owned = |a: &hsk_studio_nib::anchor_path::AnchorPath| {
            a.to_owned_path().map_err(|_| REFUSE_NETWORK)
        };
        let (mut fill, mut stroke, mut regions) = (Vec::new(), Vec::new(), Vec::new());
        for ((path, network), profile) in paths.into_iter().zip(networks).zip(profiles) {
            let Some(record) = network else {
                stroke.push((path.clone(), profile));
                fill.push(path);
                continue;
            };
            let net = nib_network(&record).map_err(|_| REFUSE_NETWORK)?;
            let filled = net.regions.iter().filter(|r| r.fills);
            for (region, g) in filled.zip(net.fill_geometry().map_err(|_| REFUSE_NETWORK)?) {
                let outline = owned(&g)?;
                match &region.fill_style_id {
                    Some(style) => regions.push((outline, style.clone())),
                    None => fill.push(outline),
                }
            }
            let s = owned(&net.stroke_geometry().map_err(|_| REFUSE_NETWORK)?)?;
            if !s.elements.is_empty() {
                stroke.push((s, profile));
            }
        }
        if let Some(e) = envelope {
            self.envelope_paints(e, &fills)?;
            if !e.distort_appearance {
                // The path geometry is wrapped first; the appearance follows the wrapped paths.
                let (n_fill, n_stroke) = (fill.len(), stroke.len());
                let mut all: Vec<OwnedPath> = std::mem::take(&mut fill);
                all.extend(stroke.iter().map(|(p, _)| p.clone()));
                all.extend(regions.iter().map(|(p, _)| p.clone()));
                let mut wrapped = self.wrap(e, &all)?.into_iter();
                fill = wrapped.by_ref().take(n_fill).collect();
                for (k, w) in wrapped.by_ref().take(n_stroke).enumerate() {
                    if let Some(s) = stroke.get_mut(k) {
                        s.0 = w;
                    }
                }
                for (r, w) in regions.iter_mut().zip(wrapped) {
                    r.0 = w;
                }
            }
        }
        let mut region_rows = Vec::new();
        for (outline, style) in regions {
            for f in self.style_rows(&style)? {
                if matches!(f.paint, FillPaint::Stroke { .. }) {
                    return Err(REFUSE_NETWORK_REGION_STYLE);
                }
                region_rows.push(FillSpec {
                    paint: FillPaint::StrokeGeometry {
                        outline: vec![outline.clone()],
                        paint: Box::new(f.paint),
                    },
                    mode: f.mode,
                    opacity: f.opacity,
                });
            }
        }
        let mut rows = Vec::with_capacity(fills.len().saturating_add(region_rows.len()));
        let mut region_rows = Some(region_rows);
        for (k, f) in fills.into_iter().enumerate() {
            match f.paint {
                FillPaint::Stroke { style, paint } => {
                    rows.extend(region_rows.take().into_iter().flatten());
                    match (brushed(k), marked(k)) {
                        (Some(_), Some(_)) => return Err(REFUSE_MARKER_BRUSH),
                        (Some(b), None) => rows.extend(self.brush_rows(b, &stroke, &paint, f.mode, f.opacity)?),
                        (None, Some(m)) => {
                            if has_network {
                                return Err(REFUSE_MARKER_NETWORK);
                            }
                            rows.push(FillSpec {
                                paint: FillPaint::StrokeGeometry {
                                    outline: self.marked_outlines(m, &stroke, &style)?,
                                    paint,
                                },
                                mode: f.mode,
                                opacity: f.opacity,
                            });
                        }
                        (None, None) => rows.push(FillSpec {
                            paint: FillPaint::StrokeGeometry {
                                outline: self.stroke_outlines(&stroke, &style)?,
                                paint,
                            },
                            mode: f.mode,
                            opacity: f.opacity,
                        }),
                    }
                }
                paint => rows.push(FillSpec { paint, mode: f.mode, opacity: f.opacity }),
            }
        }
        rows.extend(region_rows.into_iter().flatten());
        if let Some(e) = envelope.filter(|e| e.distort_appearance) {
            // The drawn appearance is wrapped: fill geometry and every own-geometry outline.
            let mut all: Vec<OwnedPath> = fill.clone();
            let mut counts = Vec::with_capacity(rows.len());
            for r in &rows {
                let n = match &r.paint {
                    FillPaint::StrokeGeometry { outline, .. } => {
                        all.extend(outline.iter().cloned());
                        outline.len()
                    }
                    _ => 0,
                };
                counts.push(n);
            }
            let mut wrapped = self.wrap(e, &all)?.into_iter();
            fill = wrapped.by_ref().take(fill.len()).collect();
            for (r, n) in rows.iter_mut().zip(counts) {
                if let FillPaint::StrokeGeometry { outline, .. } = &mut r.paint {
                    *outline = wrapped.by_ref().take(n).collect();
                }
            }
        }
        Ok((VectorGeometry::Paths(fill), rows))
    }

    /// Row index of each spec `style_rows(style_id)` returns (one per visible row, in order).
    fn visible_rows(&self, style_id: &str) -> Vec<u32> {
        use hsk_studio_folio::StudioAppearanceRow as R;
        let Some(style) = self.design.paint_styles.iter().find(|s| s.style_id == style_id) else {
            return Vec::new();
        };
        style
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r, R::Fill { visible: true, .. } | R::Stroke { visible: true, .. }))
            .filter_map(|(i, _)| u32::try_from(i).ok())
            .collect()
    }

    /// Envelope paint options composite cannot honour (it does not distort paints).
    fn envelope_paints(&self, e: &hsk_studio_folio::StudioEnvelope, fills: &[FillSpec]) -> Result<(), &'static str> {
        let has = |f: &dyn Fn(&FillPaint) -> bool| {
            fills.iter().any(|r| match &r.paint {
                FillPaint::Stroke { paint, .. } | FillPaint::StrokeGeometry { paint, .. } => f(paint),
                p => f(p),
            })
        };
        if (e.distort_linear_gradient_fills && has(&|p| matches!(p, FillPaint::Gradient(_))))
            || (e.distort_pattern_fills && has(&|p| matches!(p, FillPaint::Pattern(_))))
        {
            return Err(REFUSE_ENVELOPE_PAINT);
        }
        Ok(())
    }

    /// `paths` wrapped in envelope `e` (one frame).
    fn wrap(&self, e: &hsk_studio_folio::StudioEnvelope, paths: &[OwnedPath]) -> Result<Vec<OwnedPath>, &'static str> {
        use hsk_studio_folio::{StudioEnvelopeShape as S, StudioWarpAxis as A, StudioWarpStyle as W};
        use hsk_studio_nib::warp::{Envelope, Mesh, Warp, WarpAxis, WarpStyle, envelope_paths};
        if !self.unit_px {
            return Err(REFUSE_GEOMETRY_UNIT);
        }
        let envelope = match &e.envelope {
            S::Warp { warp_style, warp_axis, warp_bend, warp_distortion_horizontal, warp_distortion_vertical } => Envelope::Warp(Warp {
                style: match warp_style {
                    W::Arc => WarpStyle::Arc,
                    W::ArcLower => WarpStyle::ArcLower,
                    W::ArcUpper => WarpStyle::ArcUpper,
                    W::Arch => WarpStyle::Arch,
                    W::Bulge => WarpStyle::Bulge,
                    W::ShellLower => WarpStyle::ShellLower,
                    W::ShellUpper => WarpStyle::ShellUpper,
                    W::Flag => WarpStyle::Flag,
                    W::Wave => WarpStyle::Wave,
                    W::Fish => WarpStyle::Fish,
                    W::Rise => WarpStyle::Rise,
                    W::Fisheye => WarpStyle::Fisheye,
                    W::Inflate => WarpStyle::Inflate,
                    W::Squeeze => WarpStyle::Squeeze,
                    W::Twist => WarpStyle::Twist,
                },
                axis: match warp_axis {
                    A::Horizontal => WarpAxis::Horizontal,
                    A::Vertical => WarpAxis::Vertical,
                },
                bend: *warp_bend,
                distortion_horizontal: *warp_distortion_horizontal,
                distortion_vertical: *warp_distortion_vertical,
            }),
            S::Mesh { rows, columns, points } => Envelope::Mesh(Mesh {
                rows: usize::try_from(*rows).map_err(|_| REFUSE_ENVELOPE)?,
                columns: usize::try_from(*columns).map_err(|_| REFUSE_ENVELOPE)?,
                points: points.iter().map(|q| PathPoint::new(q[0], q[1])).collect(),
            }),
            S::TopObject { source_layer_id } => match self.geometry(source_layer_id, 0)? {
                VectorGeometry::Paths(top) => Envelope::Shape(top.into_iter().next().ok_or(REFUSE_ENVELOPE)?),
                VectorGeometry::Boolean { .. } => return Err(REFUSE_ENVELOPE),
            },
        };
        envelope_paths(paths, &envelope, e.envelope_fidelity).map_err(|err| match err {
            hsk_studio_nib::Error::BudgetExceeded => crate::vector_nib::REFUSE_NIB_BUDGET,
            _ => REFUSE_ENVELOPE,
        })
    }

    /// The rows of a brushed stroke row: one own-geometry row per brush art path.
    fn brush_rows(
        &self,
        b: &hsk_studio_folio::StudioBrushStroke,
        bases: &[(OwnedPath, Option<&hsk_studio_folio::StudioWidthProfile>)],
        row_paint: &FillPaint,
        mode: StudioBlendMode,
        opacity: f32,
    ) -> Result<Vec<FillSpec>, &'static str> {
        use hsk_studio_folio::{StudioArtScale as AS, StudioBrushDynamics as D, StudioBrushKind as K, StudioPatternFit as PF, StudioRotationRelative as RR};
        use hsk_studio_nib::brush::{ArtBrush, ArtScale, Dyn, PatternBrush, PatternFit, RotationRelative, ScatterBrush, TileArt, TileSlot, art_brush, pattern_brush, scatter_brush};
        if !self.unit_px {
            return Err(REFUSE_GEOMETRY_UNIT);
        }
        let brush = self.records.brush(&b.brush_id).ok_or(REFUSE_BRUSH)?;
        let k = b.scale_override.unwrap_or(100.0) / 100.0;
        let resolve = |art: &[hsk_studio_folio::StudioBrushArt]| -> Result<Vec<OwnedPath>, &'static str> {
            art.iter().map(|a| self.assets.path(&a.path_id).ok_or(REFUSE_PATH_UNRESOLVED)).collect()
        };
        // Stylus dynamics have no input in a document: the parameter's own value is used.
        let dynamic = |p: &hsk_studio_folio::StudioBrushParam, scale: f64| Dyn {
            min: p.value * scale,
            max: p.max.unwrap_or(p.value) * scale,
            random: p.dynamics == D::Random,
        };
        let nib_err = |e: hsk_studio_nib::Error| match e {
            hsk_studio_nib::Error::BudgetExceeded => crate::vector_nib::REFUSE_NIB_BUDGET,
            _ => REFUSE_BRUSH,
        };
        // Calligraphic and bristle brushes (lane SMB-ILLUSTRATOR-INTEROP) paint one shape with the
        // stroke row's own paint: nib's swept nib or bristles along every stroke path.
        let swept: Option<Sweep<'_>> = match &brush.kind {
            K::Calligraphic { nib_angle, nib_roundness, nib_size } => {
                let nb = hsk_studio_nib::brush::CalligraphicBrush { angle: nib_angle.value, roundness: nib_roundness.value, size: nib_size.value * k };
                Some(Box::new(move |spine: &OwnedPath| hsk_studio_nib::brush::calligraphic_brush(spine, &nb)))
            }
            K::Bristle { bristle_size, bristle_length, bristle_density } => {
                // bristle_size is in mm (STU-VEC-152): document px at 96 per inch.
                let nb = hsk_studio_nib::brush::BristleBrush { size: bristle_size * 96.0 / 25.4 * k, length: *bristle_length, density: *bristle_density };
                Some(Box::new(move |spine: &OwnedPath| hsk_studio_nib::brush::bristle_brush(spine, &nb)))
            }
            _ => None,
        };
        if let Some(sweep) = swept {
            let mut outline = Vec::new();
            for (spine, _) in bases {
                if spine.elements.is_empty() {
                    continue;
                }
                let out = sweep(spine).map_err(nib_err)?;
                if !out.elements.is_empty() {
                    outline.push(out);
                }
            }
            return Ok(vec![FillSpec { paint: FillPaint::StrokeGeometry { outline, paint: Box::new(row_paint.clone()) }, mode, opacity }]);
        }
        let (art, draw): (Vec<&hsk_studio_folio::StudioBrushArt>, BrushDraw<'_>) = match &brush.kind {
            K::Art { art_width, art_scale_mode, art_guide_start, art_guide_end, flip_along, flip_across, art } => {
                let paths = resolve(art)?;
                let nb = ArtBrush {
                    width: art_width.value * k,
                    scale: match art_scale_mode {
                        AS::Proportional => ArtScale::Proportional,
                        AS::StretchToStrokeLength => ArtScale::Stretch,
                        AS::StretchBetweenGuides => ArtScale::StretchBetweenGuides { start: art_guide_start.ok_or(REFUSE_BRUSH)?, end: art_guide_end.ok_or(REFUSE_BRUSH)? },
                    },
                    flip_along: *flip_along,
                    flip_across: *flip_across,
                };
                (art.iter().collect(), Box::new(move |spine: &OwnedPath| art_brush(spine, &paths, &nb)))
            }
            K::Scatter { scatter_size, scatter_spacing, scatter_offset, scatter_rotation, scatter_rotation_relative_to, seed, art } => {
                let paths = resolve(art)?;
                let nb = ScatterBrush {
                    size: dynamic(scatter_size, k),
                    spacing: dynamic(scatter_spacing, 1.0),
                    offset: dynamic(scatter_offset, 1.0),
                    rotation: dynamic(scatter_rotation, 1.0),
                    relative: match scatter_rotation_relative_to {
                        RR::Page => RotationRelative::Page,
                        RR::Path => RotationRelative::Path,
                    },
                    seed: *seed,
                };
                (art.iter().collect(), Box::new(move |spine: &OwnedPath| scatter_brush(spine, &paths, &nb)))
            }
            K::Pattern { pattern_brush_scale, fit_mode, flip_along, flip_across, tiles, .. } => {
                let mut list = Vec::new();
                let mut art = Vec::new();
                for (slot, pieces) in [(TileSlot::Side, &tiles.side), (TileSlot::OuterCorner, &tiles.outer_corner), (TileSlot::InnerCorner, &tiles.inner_corner), (TileSlot::Start, &tiles.start), (TileSlot::End, &tiles.end)] {
                    for (a, path) in pieces.iter().zip(resolve(pieces)?) {
                        list.push(TileArt { slot, path });
                        art.push(a);
                    }
                }
                let nb = PatternBrush {
                    scale: pattern_brush_scale.value * k,
                    fit: match fit_mode {
                        PF::StretchToFit => PatternFit::StretchToFit,
                        PF::AddSpaceToFit => PatternFit::AddSpaceToFit,
                        PF::ApproximatePath => PatternFit::ApproximatePath,
                    },
                    flip_along: *flip_along,
                    flip_across: *flip_across,
                };
                (art, Box::new(move |spine: &OwnedPath| pattern_brush(spine, &list, &nb)))
            }
            K::Calligraphic { .. } | K::Bristle { .. } => return Err(REFUSE_BRUSH_KIND),

        };
        let mut outlines: Vec<Vec<OwnedPath>> = vec![Vec::new(); art.len()];
        for (spine, _) in bases {
            if spine.elements.is_empty() {
                continue;
            }
            for (acc, out) in outlines.iter_mut().zip(draw(spine).map_err(nib_err)?) {
                if !out.elements.is_empty() {
                    acc.push(out);
                }
            }
        }
        let mut rows = Vec::with_capacity(art.len());
        for (a, outline) in art.iter().zip(outlines) {
            let [r, g, bl, _] = self.colour(&a.colour)?;
            let paint = match brush.colorization {
                hsk_studio_folio::StudioBrushColorization::None => FillPaint::Solid([r, g, bl, 1.0]),
                hsk_studio_folio::StudioBrushColorization::Tints => match row_paint {
                    FillPaint::Solid(c) => {
                        let l = 0.2126 * r + 0.7152 * g + 0.0722 * bl;
                        let tint = |s: f32| 1.0 - (1.0 - l) * (1.0 - s);
                        FillPaint::Solid([tint(c[0]), tint(c[1]), tint(c[2]), c[3]])
                    }
                    _ => return Err(REFUSE_BRUSH_COLORIZATION),
                },
                _ => return Err(REFUSE_BRUSH_COLORIZATION),
            };
            rows.push(FillSpec { paint: FillPaint::StrokeGeometry { outline, paint: Box::new(paint) }, mode, opacity });
        }
        Ok(rows)
    }

    /// The outline of a marked stroke row: nib's stroke outline of every base path trimmed by
    /// `markers::marker_outlines` (inside-path ends) and the marker outlines of every open subpath.
    fn marked_outlines(
        &self,
        m: &hsk_studio_folio::StudioStrokeMarkers,
        bases: &[(OwnedPath, Option<&hsk_studio_folio::StudioWidthProfile>)],
        style: &StrokeStyle,
    ) -> Result<Vec<OwnedPath>, &'static str> {
        use hsk_studio_folio::StudioArrowAlignment as FA;
        use hsk_studio_nib::markers::{MarkerAlign, MarkerEnd, MarkerError, MarkerOptions, MarkerShape, MarkerSpec, marker_outlines};
        if !self.unit_px {
            return Err(REFUSE_GEOMETRY_UNIT);
        }
        if bases.iter().any(|(_, w)| w.is_some()) {
            return Err(REFUSE_MARKER_WIDTH_PROFILE);
        }
        let code = |e: MarkerError| match e {
            MarkerError::ShapeUnbuilt => REFUSE_MARKER_SHAPE_UNBUILT,
            MarkerError::BudgetExceeded => REFUSE_NIB_BUDGET,
            MarkerError::InvalidGeometry => REFUSE_VECTOR_GEOMETRY,
            other => other.code(),
        };
        let end = |token: &str, scale: f64| -> Result<Option<MarkerEnd>, &'static str> {
            Ok(MarkerShape::from_token(token).map_err(code)?.map(|shape| MarkerEnd { shape, scale }))
        };
        let spec = MarkerSpec {
            start: end(&m.start, m.start_scale)?,
            end: end(&m.end, m.end_scale)?,
            align: match m.alignment {
                FA::InsidePath => MarkerAlign::InsidePath,
                FA::OutsidePath => MarkerAlign::OutsidePath,
            },
        };
        let mut trimmed = Vec::with_capacity(bases.len());
        let mut markers = Vec::new();
        for (path, _) in bases {
            if path.elements.is_empty() {
                continue;
            }
            let o = marker_outlines(path, &spec, style.width, &MarkerOptions::default()).map_err(code)?;
            if !o.trimmed.elements.is_empty() {
                trimmed.push(o.trimmed);
            }
            markers.extend(o.markers.into_iter().map(|p| p.outline));
        }
        let mut out: Vec<OwnedPath> =
            crate::vector_nib::stroke_outline(&VectorGeometry::Paths(trimmed), style)?
                .into_iter()
                .collect();
        out.extend(markers);
        Ok(out)
    }

    /// The stroke outlines of `bases` under `style`: uniform paths united by nib's stroker, each
    /// width-profiled path outlined by `nib::width::outline_variable` (the profile's widths replace
    /// the row weight; cap butt or round).
    fn stroke_outlines(
        &self,
        bases: &[(OwnedPath, Option<&hsk_studio_folio::StudioWidthProfile>)],
        style: &StrokeStyle,
    ) -> Result<Vec<OwnedPath>, &'static str> {
        use hsk_studio_nib::width::{MAX_WIDTH_POINTS, WidthCap, WidthPoint, WidthProfile, outline_variable};
        let uniform: Vec<OwnedPath> = bases
            .iter()
            .filter(|(_, w)| w.is_none())
            .map(|(p, _)| p.clone())
            .collect();
        let mut out: Vec<OwnedPath> =
            crate::vector_nib::stroke_outline(&VectorGeometry::Paths(uniform), style)?
                .into_iter()
                .collect();
        for (path, profile) in bases {
            let Some(w) = profile else { continue };
            if !self.unit_px {
                return Err(REFUSE_GEOMETRY_UNIT);
            }
            if w.points.len() > MAX_WIDTH_POINTS {
                return Err(REFUSE_WIDTH_PROFILE);
            }
            if !w.is_symmetric() {
                return Err(REFUSE_WIDTH_ASYMMETRIC);
            }
            let cap = match style.cap {
                LineCap::Butt => WidthCap::Butt,
                LineCap::Round => WidthCap::Round,
                LineCap::Square => return Err(REFUSE_WIDTH_STYLE),
            };
            if style.dash.is_some() || style.align != StrokeAlign::Center {
                return Err(REFUSE_WIDTH_STYLE);
            }
            let nib = WidthProfile {
                points: w.points.iter().map(|q| WidthPoint { t: q.position, width: q.width }).collect(),
            };
            nib.validate().map_err(|_| REFUSE_WIDTH_PROFILE)?;
            if path.elements.is_empty() {
                continue;
            }
            match outline_variable(path, &nib, cap, &hsk_studio_nib::boolean::BooleanOptions::default()) {
                Ok(g) => out.push(g.path),
                Err(hsk_studio_nib::Error::EmptyPathfinderResult) => {}
                Err(hsk_studio_nib::Error::BudgetExceeded) => return Err(REFUSE_NIB_BUDGET),
                Err(_) => return Err(REFUSE_VECTOR_GEOMETRY),
            }
        }
        Ok(out)
    }

    /// The fill rows of the layer's `fill_style_id` paint style (no style reference = no fills).
    fn fills(&self, layer_id: &str) -> Result<Vec<FillSpec>, &'static str> {
        let Some(style_id) = self
            .design
            .node_design(layer_id)
            .and_then(|n| n.style_refs.get("fill_style_id"))
        else {
            return Ok(Vec::new());
        };
        self.style_rows(style_id)
    }

    /// The visible rows of paint style `style_id`, bottom first (unresolved: `REFUSE_FILL_STYLE`).
    fn style_rows(&self, style_id: &str) -> Result<Vec<FillSpec>, &'static str> {
        use hsk_studio_folio::StudioAppearanceRow;
        let style = self
            .design
            .paint_styles
            .iter()
            .find(|s| s.style_id == style_id)
            .ok_or(REFUSE_FILL_STYLE)?;
        let mut out = Vec::new();
        for (row, r) in style.rows.iter().enumerate() {
            let row = u32::try_from(row).map_err(|_| REFUSE_FILL_STYLE)?;
            match r {
                StudioAppearanceRow::Stroke {
                    paint,
                    stroke,
                    opacity,
                    blend_mode,
                    visible,
                } => {
                    if !*visible {
                        continue;
                    }
                    if !self.nib {
                        return Err(REFUSE_STROKE);
                    }
                    if self.records.image_paint(style_id, row).is_some() {
                        return Err(REFUSE_STROKE_PAINT);
                    }
                    if self.records.pattern_paint(style_id, row).is_some() {
                        return Err(REFUSE_PAINT_PATTERN);
                    }
                    let mode =
                        StudioBlendMode::try_from(*blend_mode).map_err(|_| REFUSE_PAINT_MODE)?;
                    out.push(FillSpec {
                        paint: FillPaint::Stroke {
                            style: self.stroke_style(stroke)?,
                            paint: Box::new(self.paint(paint)?),
                        },
                        mode,
                        opacity: (*opacity / 100.0) as f32,
                    });
                }
                StudioAppearanceRow::Fill {
                    paint,
                    opacity,
                    blend_mode,
                    visible,
                } => {
                    if !*visible {
                        continue;
                    }
                    let mode =
                        StudioBlendMode::try_from(*blend_mode).map_err(|_| REFUSE_PAINT_MODE)?;
                    let mut opacity = (*opacity / 100.0) as f32;
                    if self.nib && self.records.pattern_paint(style_id, row).is_some() {
                        return Err(REFUSE_PAINT_PATTERN);
                    }
                    let paint = if let Some(image) = self.records.image_paint(style_id, row) {
                        let info = self
                            .assets
                            .image(&image.image)
                            .ok_or(REFUSE_IMAGE_UNRESOLVED)?;
                        opacity *= (image.opacity / 100.0) as f32;
                        use hsk_studio_folio::StudioImageScale as S;
                        FillPaint::Image(ImageFill {
                            source_id: image_source_id(&image.image),
                            info,
                            scale: match image.scale_mode {
                                S::Fill => ImageScale::Fill,
                                S::Fit => ImageScale::Fit,
                                S::Crop => ImageScale::Crop,
                                S::Tile => ImageScale::Tile,
                            },
                            transform: image.transform,
                            scaling_factor: image.scaling_factor,
                            rotation: image.rotation,
                        })
                    } else {
                        self.paint(paint)?
                    };
                    out.push(FillSpec {
                        paint,
                        mode,
                        opacity,
                    });
                }
            }
        }
        Ok(out)
    }
}

/// Prepares every vector layer of `snap` and every group layer with a live boolean record
/// (`StudioVectorRecords::boolean_groups`): the geometry through `assets` (paths) or the boolean
/// operands (cached outline first), the fills from the layer's `fill_style_id` paint style in
/// `design` (image rows through `vectors.image_paints` and `assets`), then
/// [`prepare_vector_layers`]. A layer that cannot be prepared is recorded with its refusal code
/// (strokes, gradients, unresolved paths or images, colours composite would have to convert).
pub fn prepare_vectors(
    snap: &hsk_studio_folio::Snapshot,
    design: &hsk_studio_folio::StudioDesignRecords,
    vectors: &hsk_studio_folio::StudioVectorRecords,
    assets: &dyn VectorAssets,
    space: hsk_studio_pigment::blend_math::BlendSpace,
    res: &VectorResolution,
    cancel: &CancellationToken,
) -> Result<PreparedVectors, CompositeError> {
    let doc = snap.document();
    for document_id in [&design.document_id, &vectors.document_id] {
        if *document_id != doc.document_id {
            return Err(CompositeError::UnsupportedDescriptor(
                "vector_records_document_mismatch".into(),
            ));
        }
    }
    let cx = FolioCx {
        layers: doc.layers.iter().map(|l| (l.layer_id.as_str(), l)).collect(),
        order: doc.layers.iter().collect(),
        design,
        records: vectors,
        assets,
        blending: &doc.blending_space,
        space,
        unit_px: doc.geometry_unit == hsk_studio_folio::GeometryUnit::Px,
        nib: res.coverage == VectorCoverage::Exact,
    };
    let mut specs = Vec::new();
    let mut refused: Vec<(String, &'static str)> = Vec::new();
    let mut texts = Vec::new();
    for l in &doc.layers {
        cancel.check().map_err(|_| CompositeError::Canceled)?;
        let drawn = matches!(l.payload, hsk_studio_folio::Payload::Vector { .. })
            || vectors.boolean_group(&l.layer_id).is_some();
        if !drawn {
            continue;
        }
        // A layer an envelope uses as its top object is consumed by it (STU-VEC-168: "make with
        // top object"): it draws nothing of its own.
        let consumed = vectors.envelopes.iter().any(|e| {
            matches!(&e.envelope, hsk_studio_folio::StudioEnvelopeShape::TopObject { source_layer_id } if source_layer_id == &l.layer_id)
        });
        if consumed {
            specs.push(VectorLayerSpec { layer_id: l.layer_id.clone(), geometry: VectorGeometry::Paths(Vec::new()), fills: Vec::new() });
            continue;
        }
        let prepared = cx
            .geometry(&l.layer_id, 0)
            .and_then(|geometry| Ok((geometry, cx.fills(&l.layer_id)?)))
            .and_then(|(geometry, fills)| cx.own_geometry(&l.layer_id, &l.payload, geometry, fills))
            .and_then(|(geometry, mut fills)| {
                // Live path and area text bound to this layer's paths (crate::vector_text).
                fills.extend(crate::vector_text::text_rows(vectors, &l.layer_id, assets, &mut texts)?);
                Ok((geometry, fills))
            });
        match prepared {
            Ok((geometry, fills)) => specs.push(VectorLayerSpec {
                layer_id: l.layer_id.clone(),
                geometry,
                fills,
            }),
            Err(code) => refused.push((l.layer_id.clone(), code)),
        }
    }
    let mut out = prepare_vector_layers(&specs, res, cancel)?;
    for (layer, code) in refused {
        out.push_refusal(&layer, code);
    }
    out.texts = texts;
    // Vector clips (StudioVectorClip, Figma frame clipping): the clipped layer is cut by the
    // nib outline of the source layer's geometry under the clip rule.
    for a in &vectors.attachments {
        cancel.check().map_err(|_| CompositeError::Canceled)?;
        let Some(clip_id) = &a.clip_id else { continue };
        let clip = vectors.clip(clip_id).ok_or(REFUSE_CLIP_UNRESOLVED).and_then(|c| {
            let rule = match c.clip_rule {
                hsk_studio_folio::StudioClipRule::NonZero => FillRule::NonZero,
                hsk_studio_folio::StudioClipRule::EvenOdd => FillRule::EvenOdd,
            };
            let geometry = match cx.geometry(&c.source_layer_id, 0)? {
                VectorGeometry::Paths(paths) => VectorGeometry::Paths(
                    paths
                        .into_iter()
                        .map(|mut p| {
                            p.fill_rule = rule;
                            p
                        })
                        .collect(),
                ),
                boolean => boolean,
            };
            crate::vector_nib::outline(&geometry, 0)
        });
        out.clips.push((a.layer_id.clone(), clip));
    }
    // Vector masks (StudioVectorMask, crate::vector_mask): one resolution per mask record, shared by
    // every layer attached to it; content planes are charged against one aggregate budget.
    let mut budget = crate::vector_mask::MAX_MASK_CONTENT_PIXELS;
    let mut resolved: Vec<(&str, Result<MaskOutcome, &'static str>)> = Vec::new();
    let mut attached: Vec<(String, MaskResult)> = Vec::new();
    let mut pending: Vec<crate::vector_mask::PendingMaskSource> = Vec::new();
    for a in &vectors.attachments {
        cancel.check().map_err(|_| CompositeError::Canceled)?;
        let Some(mask_id) = &a.mask_id else { continue };
        let outcome = match resolved.iter().find(|(id, _)| *id == mask_id.as_str()) {
            Some((_, m)) => m.clone(),
            None => {
                let m = cx.vector_mask(mask_id, &out, doc.geometry_unit, &mut budget);
                resolved.push((mask_id.as_str(), m.clone()));
                m
            }
        };
        let mask = match outcome {
            Ok(MaskOutcome::Ready(m)) => Ok(m),
            Ok(MaskOutcome::Pending(p)) => {
                match pending.iter_mut().find(|q| q.mask_id == p.mask_id) {
                    Some(q) => q.layers.push(a.layer_id.clone()),
                    None => {
                        let mut p = *p;
                        p.layers.push(a.layer_id.clone());
                        pending.push(p);
                    }
                }
                Err(crate::vector_mask::REFUSE_MASK_SOURCE_NOT_RENDERED)
            }
            Err(code) => Err(code),
        };
        attached.push((a.layer_id.clone(), mask));
    }
    out.masks.extend(attached);
    out.pending.extend(pending);
    Ok(out)
}

type MaskResult = Result<std::sync::Arc<crate::vector_mask::VectorMask>, &'static str>;

/// A resolved mask record: ready, or waiting for the caller's backend to render its source.
#[derive(Clone)]
enum MaskOutcome {
    Ready(std::sync::Arc<crate::vector_mask::VectorMask>),
    Pending(Box<crate::vector_mask::PendingMaskSource>),
}

impl FolioCx<'_> {
    /// Is `layer` masked itself (a vector mask attachment, an enabled raster mask or the
    /// clip-to-base route), or a group with a vector clip? A vector clip on a leaf (vector layer or
    /// boolean group) is applied to its content (`crate::vector_mask::mask_content`; e.g. a Figma
    /// mask shape inside a clipping frame).
    fn nested_mask(&self, layer: &hsk_studio_folio::StudioLayer, leaf: bool) -> bool {
        let a = layer.effective_appearance();
        a.clipped
            || a.mask.as_ref().is_some_and(|m| m.enabled)
            || self.records.attachments.iter().any(|t| {
                t.layer_id == layer.layer_id && (t.mask_id.is_some() || (t.clip_id.is_some() && !leaf))
            })
    }
    /// The visible children of group `layer_id`, Folio layer order (bottom first).
    fn children(&self, layer_id: &str) -> Vec<&hsk_studio_folio::StudioLayer> {
        self.order
            .iter()
            .copied()
            .filter(|l| l.visible)
            .filter(|l| matches!(&l.parent, hsk_studio_folio::Parent::Node(p) if p.node_id == layer_id))
            .collect()
    }
    /// The geometry of a `VECTOR` mask source: a vector layer or boolean group, or a plain group
    /// (the union of its visible members' geometry).
    fn mask_geometry(
        &self,
        layer_id: &str,
        depth: usize,
        members: &mut usize,
    ) -> Result<VectorGeometry, &'static str> {
        use crate::vector_mask::{MAX_MASK_MEMBERS, REFUSE_MASK_BUDGET, REFUSE_MASK_SOURCE_KIND};
        *members = members
            .checked_add(1)
            .filter(|m| *m <= MAX_MASK_MEMBERS)
            .ok_or(REFUSE_MASK_BUDGET)?;
        if depth > MAX_BOOLEAN_DEPTH {
            return Err(REFUSE_MASK_BUDGET);
        }
        let layer = self
            .layers
            .get(layer_id)
            .ok_or(crate::vector_mask::REFUSE_MASK_UNRESOLVED)?;
        let boolean = self.records.boolean_group(layer_id).is_some();
        match &layer.payload {
            hsk_studio_folio::Payload::Vector { .. } => self.geometry(layer_id, 0),
            hsk_studio_folio::Payload::Group if boolean => self.geometry(layer_id, 0),
            hsk_studio_folio::Payload::Group => {
                let next = depth.checked_add(1).ok_or(REFUSE_MASK_BUDGET)?;
                let operands = self
                    .children(layer_id)
                    .into_iter()
                    .map(|c| self.mask_geometry(&c.layer_id, next, members))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(VectorGeometry::Boolean {
                    op: BooleanOp::Union,
                    operands,
                })
            }
            _ => Err(REFUSE_MASK_SOURCE_KIND),
        }
    }
    /// The content tree of an `ALPHA` / `LUMINANCE` mask source (`crate::vector_mask::MaskNode`).
    fn mask_node(
        &self,
        layer_id: &str,
        depth: usize,
        members: &mut usize,
    ) -> Result<crate::vector_mask::MaskNode, &'static str> {
        use crate::vector_mask::{
            MAX_MASK_MEMBERS, MaskNode, REFUSE_MASK_BUDGET, REFUSE_MASK_SOURCE_KIND,
            REFUSE_MASK_SOURCE_NESTED,
        };
        *members = members
            .checked_add(1)
            .filter(|m| *m <= MAX_MASK_MEMBERS)
            .ok_or(REFUSE_MASK_BUDGET)?;
        if depth > MAX_BOOLEAN_DEPTH {
            return Err(REFUSE_MASK_BUDGET);
        }
        let layer = self
            .layers
            .get(layer_id)
            .ok_or(crate::vector_mask::REFUSE_MASK_UNRESOLVED)?;
        let boolean = self.records.boolean_group(layer_id).is_some();
        let leaf = boolean || matches!(layer.payload, hsk_studio_folio::Payload::Vector { .. });
        if self.nested_mask(layer, leaf) {
            return Err(REFUSE_MASK_SOURCE_NESTED);
        }
        let a = layer.effective_appearance();
        let mode = match StudioBlendMode::try_from(a.blend_mode).map_err(|_| REFUSE_PAINT_MODE)? {
            // A member group is evaluated isolated (its own result composited Normal).
            StudioBlendMode::PassThrough => StudioBlendMode::Normal,
            m => m,
        };
        let opacity = ((a.opacity / 100.0) * (a.fill_opacity / 100.0)) as f32;
        if !(0.0..=1.0).contains(&opacity) {
            return Err(REFUSE_PAINT_MODE);
        }
        let make_leaf = || MaskNode::Leaf {
            layer_id: layer_id.to_owned(),
            mode,
            opacity,
        };
        match &layer.payload {
            hsk_studio_folio::Payload::Vector { .. } => Ok(make_leaf()),
            hsk_studio_folio::Payload::Group if boolean => Ok(make_leaf()),
            hsk_studio_folio::Payload::Group => {
                let next = depth.checked_add(1).ok_or(REFUSE_MASK_BUDGET)?;
                let children = self
                    .children(layer_id)
                    .into_iter()
                    .map(|c| self.mask_node(&c.layer_id, next, members))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(MaskNode::Group {
                    mode,
                    opacity,
                    children,
                })
            }
            _ => Err(REFUSE_MASK_SOURCE_KIND),
        }
    }
    /// Resolve mask record `mask_id` (crate::vector_mask module docs).
    fn vector_mask(
        &self,
        mask_id: &str,
        prepared: &PreparedVectors,
        unit: hsk_studio_folio::GeometryUnit,
        budget: &mut u64,
    ) -> Result<MaskOutcome, &'static str> {
        use crate::vector_mask::{
            MaskContentMode, PendingMaskSource, REFUSE_MASK_REGION, REFUSE_MASK_REVEAL_MODE,
            REFUSE_MASK_SOURCE_IMAGE, REFUSE_MASK_SOURCE_KIND, REFUSE_MASK_UNRESOLVED, VectorMask,
            VectorMaskSource,
        };
        use hsk_studio_folio::StudioMaskMode;
        let rec = self
            .records
            .masks
            .iter()
            .find(|m| m.mask_id == mask_id)
            .ok_or(REFUSE_MASK_UNRESOLVED)?;
        if !self.layers.contains_key(rec.source_layer_id.as_str()) {
            return Err(REFUSE_MASK_UNRESOLVED);
        }
        let region = match &rec.region {
            None => None,
            Some(b) => {
                let v = |l: &hsk_studio_folio::Length| {
                    if l.unit == unit && l.value.is_finite() {
                        Ok(l.value)
                    } else {
                        Err(REFUSE_MASK_REGION)
                    }
                };
                let (x, y, w, h) = (v(&b.x)?, v(&b.y)?, v(&b.width)?, v(&b.height)?);
                if w < 0.0 || h < 0.0 || !(x + w).is_finite() || !(y + h).is_finite() {
                    return Err(REFUSE_MASK_REGION);
                }
                Some([x, y, x + w, y + h])
            }
        };
        // Parameters (Folio StudioVectorMaskParameters, lane SMB-COMPOSITE-VECTOR-MASK-2); Folio
        // validates the ranges, these are defence in depth.
        let params = self.records.mask_parameters(mask_id);
        let density = params.map_or(100.0, |p| p.density);
        let feather = params.map_or(0.0, |p| p.feather);
        if !(0.0..=100.0).contains(&density) || !(0.0..=10_000.0).contains(&feather) {
            return Err(REFUSE_MASK_UNRESOLVED);
        }
        let reveal = params.is_some_and(|p| p.reveal_outside);
        let mode = match (rec.mode, reveal) {
            (StudioMaskMode::Luminance, true) => Some(MaskContentMode::LuminanceRevealed),
            (_, true) => return Err(REFUSE_MASK_REVEAL_MODE),
            (StudioMaskMode::Alpha, false) => Some(MaskContentMode::Alpha),
            (StudioMaskMode::Luminance, false) => Some(MaskContentMode::Luminance),
            (StudioMaskMode::Vector, false) => None,
        };
        let template = |source| VectorMask {
            source,
            invert: rec.invert_mask,
            region,
            density: (density / 100.0) as f32,
            feather_px: feather as f32,
            linked: params.is_none_or(|p| p.linked),
        };
        let mut members = 0usize;
        let source = match mode {
            None => {
                let g = self.mask_geometry(&rec.source_layer_id, 0, &mut members)?;
                VectorMaskSource::Outline(crate::vector_nib::outline(&g, 0)?)
            }
            Some(mode) => {
                let content = self
                    .mask_node(&rec.source_layer_id, 0, &mut members)
                    .and_then(|node| crate::vector_mask::mask_content(prepared, &node, mode, budget));
                match content {
                    Ok(c) => c,
                    // Raster, text or image-paint content: the caller's backend renders the source
                    // (crate::vector_mask module docs).
                    Err(REFUSE_MASK_SOURCE_KIND | REFUSE_MASK_SOURCE_IMAGE) => {
                        return Ok(MaskOutcome::Pending(Box::new(PendingMaskSource {
                            mask_id: mask_id.to_owned(),
                            source_layer_id: rec.source_layer_id.clone(),
                            mode,
                            layers: Vec::new(),
                            template: template(VectorMaskSource::Content {
                                rect: None,
                                values: Vec::new(),
                                outside: mode.outside(),
                            }),
                        })));
                    }
                    Err(code) => return Err(code),
                }
            }
        };
        let mask = template(source);
        // A non-finite or out-of-range source is refused here, so evaluation reports it.
        crate::vector_mask::source_bounds(&mask)?;
        Ok(MaskOutcome::Ready(std::sync::Arc::new(mask)))
    }
}

/// One painted shape of an expanded vector layer: filled `paths` (each under its own fill rule)
/// painted with `paint` (solid, gradient, pattern or image) under `mode` and `opacity`.
#[derive(Clone, Debug, PartialEq)]
pub struct ExpandedRow {
    pub paths: Vec<OwnedPath>,
    pub paint: FillPaint,
    pub mode: StudioBlendMode,
    pub opacity: f32,
}

/// The expanded layers and the refused ones (layer id, refusal code).
pub type Expanded = (Vec<ExpandedLayer>, Vec<(String, &'static str)>);

/// A vector layer as plain filled shapes, bottom first (lane SMB-ILLUSTRATOR-INTEROP, export).
#[derive(Clone, Debug, PartialEq)]
pub struct ExpandedLayer {
    pub layer_id: String,
    pub rows: Vec<ExpandedRow>,
}

/// Every vector layer of `snap` the way [`prepare_vectors`] draws it, expanded into plain filled
/// shapes for export: fill rows over the layer geometry (a boolean group: its nib outline), stroke
/// rows as their nib stroke outline, own-geometry rows (networks, region styles, width profiles,
/// brushes) as their outlines, envelopes applied. Swatch-bound rows carry their stand-in paint.
/// Vector clips and masks are NOT applied (the caller reports them); a refused layer is listed
/// with its code instead.
pub fn expand_vectors(
    snap: &hsk_studio_folio::Snapshot,
    design: &hsk_studio_folio::StudioDesignRecords,
    vectors: &hsk_studio_folio::StudioVectorRecords,
    assets: &dyn VectorAssets,
    space: hsk_studio_pigment::blend_math::BlendSpace,
    res: &VectorResolution,
    cancel: &CancellationToken,
) -> Result<Expanded, CompositeError> {
    let doc = snap.document();
    for document_id in [&design.document_id, &vectors.document_id] {
        if *document_id != doc.document_id {
            return Err(CompositeError::UnsupportedDescriptor("vector_records_document_mismatch".into()));
        }
    }
    let cx = FolioCx {
        layers: doc.layers.iter().map(|l| (l.layer_id.as_str(), l)).collect(),
        order: doc.layers.iter().collect(),
        design,
        records: vectors,
        assets,
        blending: &doc.blending_space,
        space,
        unit_px: doc.geometry_unit == hsk_studio_folio::GeometryUnit::Px,
        nib: res.coverage == VectorCoverage::Exact,
    };
    let (mut out, mut refused) = (Vec::new(), Vec::new());
    for l in &doc.layers {
        cancel.check().map_err(|_| CompositeError::Canceled)?;
        let drawn = matches!(l.payload, hsk_studio_folio::Payload::Vector { .. }) || vectors.boolean_group(&l.layer_id).is_some();
        let consumed = vectors.envelopes.iter().any(|e| matches!(&e.envelope, hsk_studio_folio::StudioEnvelopeShape::TopObject { source_layer_id } if source_layer_id == &l.layer_id));
        if !drawn || consumed {
            continue;
        }
        let layer = cx
            .geometry(&l.layer_id, 0)
            .and_then(|geometry| Ok((geometry, cx.fills(&l.layer_id)?)))
            .and_then(|(geometry, fills)| cx.own_geometry(&l.layer_id, &l.payload, geometry, fills))
            .and_then(|(geometry, mut fills)| {
                // Live path and area text as outlines (crate::vector_text; a story the caller does
                // not shape is not exported, as it is not drawn).
                fills.extend(crate::vector_text::text_rows(vectors, &l.layer_id, assets, &mut Vec::new())?);
                Ok((geometry, fills))
            })
            .and_then(|(geometry, fills)| {
                let base: Vec<OwnedPath> = match &geometry {
                    VectorGeometry::Paths(p) => p.clone(),
                    VectorGeometry::Boolean { .. } => crate::vector_nib::outline(&geometry, 0)?.into_iter().collect(),
                };
                let mut rows = Vec::with_capacity(fills.len());
                for f in fills {
                    let (paths, paint) = match f.paint {
                        FillPaint::Stroke { style, paint } => (crate::vector_nib::stroke_outline(&geometry, &style)?.into_iter().collect(), *paint),
                        FillPaint::StrokeGeometry { outline, paint } => (outline, *paint),
                        paint => (base.clone(), paint),
                    };
                    rows.push(ExpandedRow { paths, paint, mode: f.mode, opacity: f.opacity });
                }
                Ok(rows)
            });
        match layer {
            Ok(rows) => out.push(ExpandedLayer { layer_id: l.layer_id.clone(), rows }),
            Err(code) => refused.push((l.layer_id.clone(), code)),
        }
    }
    Ok((out, refused))
}
