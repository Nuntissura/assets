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

#[derive(Clone, Copy, Debug)]
pub struct VectorResolution {
    /// Samples per axis, `1..=16`.
    pub aa_samples: u8,
    /// Pixels of one layer plane, checked before it is allocated.
    pub max_layer_pixels: u64,
    pub path_limits: PathLimits,
}
impl Default for VectorResolution {
    fn default() -> Self {
        Self {
            aa_samples: 4,
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
    pub fills: Vec<PreparedFill>,
}
impl VectorPlane {
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
}
impl PreparedVectors {
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

/// Draws one spec: the coverage plane and the prepared fill rows, or the refusal code.
fn prepare_one(
    spec: &VectorLayerSpec,
    res: &VectorResolution,
    cancel: &CancellationToken,
) -> Result<Result<VectorPlane, &'static str>, CompositeError> {
    let built = match build(&spec.geometry, &res.path_limits, 0) {
        Ok(b) => b,
        Err(code) => return Ok(Err(code)),
    };
    let mut fills = Vec::with_capacity(spec.fills.len());
    for f in &spec.fills {
        if !(f.opacity.is_finite() && (0.0..=1.0).contains(&f.opacity)) {
            return Ok(Err(REFUSE_FILL));
        }
        let paint = match &f.paint {
            FillPaint::Solid(rgba) => {
                if !rgba.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c)) {
                    return Ok(Err(REFUSE_FILL));
                }
                PreparedPaint::Solid(*rgba)
            }
            FillPaint::Image(image) => {
                let Some(bbox) = built.bbox else {
                    return Ok(Err(REFUSE_IMAGE_BOUNDS));
                };
                match placements(image, bbox) {
                    Ok(placements) => PreparedPaint::Image {
                        source_id: image.source_id.clone(),
                        info: image.info,
                        placements,
                    },
                    Err(code) => return Ok(Err(code)),
                }
            }
        };
        fills.push(PreparedFill {
            paint,
            mode: f.mode,
            opacity: f.opacity,
        });
    }
    let empty = || VectorPlane {
        layer_id: spec.layer_id.clone(),
        placement: None,
        coverage: Vec::new(),
        sha256: hsk_studio_prism::profile_hash(&[]),
        fills: Vec::new(),
    };
    let Some(bbox) = built.bbox else {
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
    for fill in &fills {
        if let PreparedPaint::Image {
            info, placements, ..
        } = &fill.paint
        {
            for a in placements {
                let t = LayerTransform::new(*a, IMAGE_FILTER).with_edge(EdgeMode::Clamp);
                let source = Rect {
                    x: 0,
                    y: 0,
                    width: info.width,
                    height: info.height,
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
    }
    let (x0, y0, width, height) = (
        plane_rect.x as f64,
        plane_rect.y as f64,
        plane_rect.width,
        plane_rect.height,
    );
    let Some(pixels) = u64::from(width)
        .checked_mul(u64::from(height))
        .filter(|p| *p <= res.max_layer_pixels)
    else {
        return Ok(Err(REFUSE_VECTOR_OVER_LIMIT));
    };
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
    let step = 1.0 / f64::from(res.aa_samples);
    let half = samples.checked_div(2).ok_or(CompositeError::Overflow)?;
    for py in 0..height {
        cancel.check().map_err(|_| CompositeError::Canceled)?;
        for px in 0..width {
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
    Ok(Ok(VectorPlane {
        layer_id: spec.layer_id.clone(),
        placement: Some(plane_rect),
        sha256: hsk_studio_prism::profile_hash(&coverage),
        coverage,
        fills,
    }))
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

/// Caller-owned resolution of what Folio references by id: `SVPT` path geometry (in plan pixels:
/// the caller applies the document unit and resolution) and decoded image paint bytes.
pub trait VectorAssets {
    fn path(&self, path_id: &str) -> Option<OwnedPath>;
    /// Info of the decoded image of `source` (Folio `StudioImageSource`, bound by sha256).
    fn image(&self, source: &hsk_studio_folio::StudioImageSource) -> Option<ImageInfo>;
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

struct FolioCx<'a> {
    layers: std::collections::BTreeMap<&'a str, &'a hsk_studio_folio::StudioLayer>,
    design: &'a hsk_studio_folio::StudioDesignRecords,
    records: &'a hsk_studio_folio::StudioVectorRecords,
    assets: &'a dyn VectorAssets,
    blending: &'a hsk_studio_folio::ProfileBinding,
    space: hsk_studio_pigment::blend_math::BlendSpace,
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
        let rgb = [c.r as f32, c.g as f32, c.b as f32];
        if self.space == hsk_studio_pigment::blend_math::BlendSpace::LinearLight
            && rgb.iter().any(|v| *v != 0.0 && *v != 1.0)
        {
            return Err(REFUSE_PAINT_LINEAR);
        }
        Ok([rgb[0], rgb[1], rgb[2], 1.0])
    }

    /// The fill rows of the layer's `fill_style_id` paint style (no style reference = no fills).
    fn fills(&self, layer_id: &str) -> Result<Vec<FillSpec>, &'static str> {
        use hsk_studio_folio::{StudioAppearanceRow, StudioPaint};
        let Some(style_id) = self
            .design
            .node_design(layer_id)
            .and_then(|n| n.style_refs.get("fill_style_id"))
        else {
            return Ok(Vec::new());
        };
        let style = self
            .design
            .paint_styles
            .iter()
            .find(|s| &s.style_id == style_id)
            .ok_or(REFUSE_FILL_STYLE)?;
        let mut out = Vec::new();
        for (row, r) in style.rows.iter().enumerate() {
            let row = u32::try_from(row).map_err(|_| REFUSE_FILL_STYLE)?;
            match r {
                StudioAppearanceRow::Stroke { visible, .. } => {
                    if *visible {
                        return Err(REFUSE_STROKE);
                    }
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
                        match paint {
                            StudioPaint::Solid { colour } => FillPaint::Solid(self.colour(colour)?),
                            StudioPaint::Gradient { .. } => return Err(REFUSE_PAINT_GRADIENT),
                        }
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
        design,
        records: vectors,
        assets,
        blending: &doc.blending_space,
        space,
    };
    let mut specs = Vec::new();
    let mut refused: Vec<(String, &'static str)> = Vec::new();
    for l in &doc.layers {
        cancel.check().map_err(|_| CompositeError::Canceled)?;
        let drawn = matches!(l.payload, hsk_studio_folio::Payload::Vector { .. })
            || vectors.boolean_group(&l.layer_id).is_some();
        if !drawn {
            continue;
        }
        let prepared = cx
            .geometry(&l.layer_id, 0)
            .and_then(|geometry| Ok((geometry, cx.fills(&l.layer_id)?)));
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
    Ok(out)
}
