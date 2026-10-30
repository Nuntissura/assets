//! Raster fill tools (lane SMB-PS-GRADIENT-BUCKET-1; STU-RAS-025, STU-RAS-160, STU-RAS-161,
//! STU-RAS-119): the Gradient tool, the Paint Bucket (flood fill) and Edit > Fill of a selection.
//!
//! Pure compute: no ports, no leases, no Folio writes. Every op returns a [`FillResult`]: the
//! replacement colour and alpha planes plus a coverage plane, the inputs of the existing
//! [`crate::masked_replace`] route the brush uses (`result = lerp(before, replacement, coverage)`,
//! alpha-aware). The caller publishes them; pigment never writes document state.
//!
//! Values are straight colour and alpha in the caller's space (the views' encoding); the stop
//! colours, the pattern pixels and the blend run in that same space. Tile values outside `[0, 1]`
//! are refused by the blend (`blend_out_of_domain`), never clamped silently.
//!
//! Studio readings (class Approximate, Photoshop behaviour UNVERIFIED, oracle list D5):
//! - Drag-line geometry, `d = to - from`, `L = |d|`, `v` = pixel centre `(x + 0.5, y + 0.5)` minus
//!   `from`, `t` clamped to `[0, 1]`: linear `t = v.d / L^2`; reflected `|v.d| / L^2`; radial
//!   `|v| / L`; diamond `(|v.u| + |v.n|) / L` (`u = d / L`, `n` its perpendicular: a square
//!   rotated so one vertex points along the drag); angle: the angle from `d` to `v` measured
//!   counter-clockwise on screen (y down), one turn = 1 (`v = 0`: `t = 0`).
//! - Stops: the location axis is the STU-RAS-160 integer `0..=4096`; `t <= first` takes the first
//!   stop, `t >= last` the last. Between stops `a` and `b` the local fraction `f` is remapped by
//!   `a`'s midpoint `m` (percent / 100) piecewise linearly so `f = m` gives 0.5 (`f < m`:
//!   `0.5 f / m`; else `0.5 + 0.5 (f - m) / (1 - m)`), then interpolated linearly by the gradient
//!   overlay's own stop interpolation (`effect_math::style::gradient_sample`, one definition). The
//!   colour list and the transparency list are sampled independently (STU-RAS-160).
//! - `interpolation` (smoothness) other than 4096, the `noise` form and the `shape_burst` geometry
//!   are refused with typed codes (declared gaps, never dropped).
//! - Dither: per channel `c + (u - 0.5) / 255`, `u` the deterministic site value of
//!   [`crate::blend_math::site_unit`] at `(x, y)` with seed `dither_seed ^ channel`, clamped to
//!   `[0, 1]`; colour only (STU-CON-007 (c): seeded and deterministic).
//! - Blend: [`crate::blend_math::composite_over`] with the paint opacity (W3C source-over with the
//!   mode); the tool modes `behind` (the old pixel over the paint) and `clear` (alpha times
//!   `1 - paint alpha x opacity`) are computed here. Group-only and unrecovered modes refuse typed.
//! - Flood region: [`crate::select_tools::wand::magic_wand`] (one flood definition; its tolerance
//!   metric and its 3x3 anti-alias kernel), intersected with the selection.
//! - Pattern: tiled from `origin` (document pixels), nearest pixel, through
//!   [`crate::effect_math::PatternOverlayOp::sample_at`] (scale 1, not linked to the layer).
#![deny(clippy::arithmetic_side_effects)]
use crate::blend_math::{BlendError, StudioBlendMode};
use crate::select_tools::{ToolError, ToolLimits};
use crate::selection::{ColourView, CoveragePlane};
use crate::{Error, Rect};
use std::fmt;

/// Stops one list may carry (a Pigment ceiling; STU-RAS-160 observed 20 transparency stops).
pub const MAX_FILL_STOPS: usize = 256;
/// End of the STU-RAS-160 position axis.
pub const LOCATION_END: u16 = 4096;
/// The only admitted `interpolation` (smoothness) value: 4096, observed on every shipped
/// stop-form gradient (STU-RAS-160).
pub const SMOOTHNESS_FULL: u16 = 4096;
/// Largest absolute drag or seed coordinate in document pixels.
pub const MAX_COORDINATE: f64 = 1.0e9;
/// Largest absolute pattern origin coordinate (the pattern overlay's phase bound).
pub const MAX_PATTERN_ORIGIN: i64 = 1_000_000;

/// Typed refusal of a fill op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillError {
    Pigment(Error),
    Tool(ToolError),
    Blend(BlendError),
    /// The drag start equals its end.
    ZeroLengthDrag,
    /// A coordinate beyond [`MAX_COORDINATE`].
    CoordinateOutOfRange,
    /// The bucket seed is outside the canvas.
    SeedOutsideCanvas,
    /// A stop list is empty.
    TooFewStops,
    /// A stop list is longer than [`MAX_FILL_STOPS`].
    TooManyStops,
    /// Stop locations decrease.
    StopsOutOfOrder,
    /// A stop location, midpoint, colour or opacity is outside its STU-RAS-160 range.
    StopOutOfRange,
    /// The tool opacity is outside `[0, 1]`.
    OpacityOutOfRange,
    /// `form = noise` (declared gap).
    NoiseUnsupported,
    /// `geometry = shape_burst` (declared gap).
    ShapeBurstUnsupported,
    /// `interpolation` other than [`SMOOTHNESS_FULL`] (declared gap).
    SmoothnessUnsupported,
    /// A pattern of zero width or height.
    PatternEmpty,
    /// A pattern side above `MAX_PATTERN_SIDE`.
    PatternTooLarge,
    /// Pattern pixels not `width x height`, or a pixel outside `[0, 1]`.
    PatternShape,
    /// A pattern reference without an id (the id is authoritative, STU-RAS-161).
    PatternIdMissing,
    /// The pattern origin is beyond [`MAX_PATTERN_ORIGIN`].
    PatternOriginOutOfRange,
    /// The region to fill is empty (selection, canvas and layer view do not meet).
    NothingToFill,
}
impl FillError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Pigment(e) => e.code(),
            Self::Tool(e) => e.code(),
            Self::Blend(e) => e.code(),
            Self::ZeroLengthDrag => "zero_length_drag",
            Self::CoordinateOutOfRange => "coordinate_out_of_range",
            Self::SeedOutsideCanvas => "seed_outside_canvas",
            Self::TooFewStops => "too_few_stops",
            Self::TooManyStops => "too_many_stops",
            Self::StopsOutOfOrder => "stops_out_of_order",
            Self::StopOutOfRange => "stop_out_of_range",
            Self::OpacityOutOfRange => "opacity_out_of_range",
            Self::NoiseUnsupported => "gradient_noise_unsupported",
            Self::ShapeBurstUnsupported => "gradient_shape_burst_unsupported",
            Self::SmoothnessUnsupported => "gradient_smoothness_unsupported",
            Self::PatternEmpty => "pattern_empty",
            Self::PatternTooLarge => "pattern_too_large",
            Self::PatternShape => "pattern_shape",
            Self::PatternIdMissing => "pattern_id_missing",
            Self::PatternOriginOutOfRange => "pattern_origin_out_of_range",
            Self::NothingToFill => "nothing_to_fill",
        }
    }
}
impl From<Error> for FillError {
    fn from(e: Error) -> Self {
        Self::Pigment(e)
    }
}
impl From<ToolError> for FillError {
    fn from(e: ToolError) -> Self {
        Self::Tool(e)
    }
}
impl From<BlendError> for FillError {
    fn from(e: BlendError) -> Self {
        Self::Blend(e)
    }
}
impl From<crate::selection::SelectionError> for FillError {
    fn from(e: crate::selection::SelectionError) -> Self {
        Self::Tool(ToolError::Selection(e))
    }
}
impl fmt::Display for FillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for FillError {}

/// STU-RAS-160 `form`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GradientForm {
    #[default]
    Stops,
    Noise,
}

/// STU-RAS-160 `geometry`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GradientGeometry {
    #[default]
    Linear,
    Radial,
    Angle,
    Reflected,
    Diamond,
    ShapeBurst,
}
impl GradientGeometry {
    pub const ALL: [Self; 6] = [Self::Linear, Self::Radial, Self::Angle, Self::Reflected, Self::Diamond, Self::ShapeBurst];
    pub const fn key(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Radial => "radial",
            Self::Angle => "angle",
            Self::Reflected => "reflected",
            Self::Diamond => "diamond",
            Self::ShapeBurst => "shape_burst",
        }
    }
}

/// A colour stop: `location` on the `0..=4096` axis, `midpoint` percent `[0, 100]` toward the
/// next stop (default 50), straight colour `[0, 1]` (the caller resolves `colour_type`
/// foreground / background to a colour).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColourStop {
    pub location: u16,
    pub midpoint: f32,
    pub colour: [f32; 3],
}

/// A transparency stop: `opacity` percent `[0, 100]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransparencyStop {
    pub location: u16,
    pub midpoint: f32,
    pub opacity: f32,
}

/// The STU-RAS-160 gradient a raster gradient fill draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterGradient<'a> {
    pub form: GradientForm,
    pub geometry: GradientGeometry,
    pub colour_stops: &'a [ColourStop],
    pub transparency_stops: &'a [TransparencyStop],
    /// `interpolation` (smoothness), `0..=4096`; only 4096 is admitted.
    pub smoothness: u16,
    pub reverse: bool,
    pub dither: bool,
    pub dither_seed: u64,
}

/// The drag of the gradient tool, document pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragLine {
    pub from: [f64; 2],
    pub to: [f64; 2],
}

/// Blend mode and opacity (`[0, 1]`) of the fill; `seed` drives `dissolve`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FillPaint {
    pub mode: StudioBlendMode,
    pub opacity: f32,
    pub seed: u64,
}
impl Default for FillPaint {
    fn default() -> Self {
        Self { mode: StudioBlendMode::Normal, opacity: 1.0, seed: 0 }
    }
}

/// A pattern tile given as pixels by the caller (STU-RAS-161): straight RGBA `[0, 1]`, row-major,
/// `width x height` (grayscale and indexed patterns are expanded by the caller), tiled from
/// `origin` in document pixels. `id` is authoritative, `name` a label.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PatternTile<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub width: u32,
    pub height: u32,
    pub pixels: &'a [[f32; 4]],
    pub origin: [i64; 2],
}

/// What a bucket or Edit > Fill paints.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FillContent<'a> {
    /// An opaque colour.
    Colour([f32; 3]),
    Pattern(PatternTile<'a>),
}

/// Flood options (STU-RAS-119 defaults: contiguous and anti-alias on; tolerance 32 is the Studio
/// default of the magic wand, Photoshop's UNVERIFIED).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloodOptions {
    pub tolerance: f64,
    pub contiguous: bool,
    pub anti_alias: bool,
}
impl Default for FloodOptions {
    fn default() -> Self {
        Self { tolerance: 32.0, contiguous: true, anti_alias: true }
    }
}

/// The output of a fill op over `bounds`: replacement `rgb_f32le` colour and `f32le` straight
/// alpha (tight, row-major) and the coverage plane (same bounds) to feed `masked_replace`. Where
/// the coverage is 0 the replacement bytes are the view's own.
#[derive(Clone, Debug, PartialEq)]
pub struct FillResult {
    pub bounds: Rect,
    pub colour: Vec<u8>,
    pub alpha: Vec<u8>,
    pub coverage: CoveragePlane,
}

/// Raster gradient over `target` (the layer's pixels), clipped to `canvas` and `selection` (`None`
/// = everything).
pub fn gradient_fill(
    target: &ColourView<'_>,
    selection: Option<&CoveragePlane>,
    canvas: Rect,
    drag: DragLine,
    gradient: &RasterGradient<'_>,
    paint: &FillPaint,
    limits: &ToolLimits,
) -> Result<FillResult, FillError> {
    let _ = (target, selection, canvas, drag, gradient, paint, limits);
    Err(FillError::Pigment(Error::Unsupported))
}

/// Paint bucket: floods from `seed` over `sample` (the active layer, or a caller-supplied merged
/// frame; `None` = `target`) and fills the region of `target`, clipped to the selection.
#[allow(clippy::too_many_arguments)]
pub fn flood_fill(
    target: &ColourView<'_>,
    sample: Option<&ColourView<'_>>,
    seed: [f64; 2],
    opts: &FloodOptions,
    content: &FillContent<'_>,
    paint: &FillPaint,
    selection: Option<&CoveragePlane>,
    canvas: Rect,
    limits: &ToolLimits,
) -> Result<FillResult, FillError> {
    let _ = (target, sample, seed, opts, content, paint, selection, canvas, limits);
    Err(FillError::Pigment(Error::Unsupported))
}

/// Edit > Fill: fills the selection (`None` = the whole canvas) of `target` with `content`.
pub fn fill_selection(
    target: &ColourView<'_>,
    selection: Option<&CoveragePlane>,
    canvas: Rect,
    content: &FillContent<'_>,
    paint: &FillPaint,
    limits: &ToolLimits,
) -> Result<FillResult, FillError> {
    let _ = (target, selection, canvas, content, paint, limits);
    Err(FillError::Pigment(Error::Unsupported))
}

/// Fill-tool surface description (Studio readings, parity NOT_PROVEN).
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"fill_tools","version":1,"ops":["gradient_fill","flood_fill","fill_selection"],"output":"replacement rgb_f32le + straight f32le alpha + coverage plane for masked_replace; no document writes","gradient":"independent colour and transparency stop lists (STU-RAS-160), location 0..4096, midpoint piecewise-linear (f = m gives 0.5), segment interpolation = effect_math gradient_sample; geometries linear, radial, angle, reflected, diamond from a drag line (see module doc); reverse; dither seeded per site (+-0.5/255)","refused":["form noise: gradient_noise_unsupported","geometry shape_burst: gradient_shape_burst_unsupported","interpolation != 4096: gradient_smoothness_unsupported"],"flood":"region = select_tools::wand::magic_wand (tolerance, contiguous, anti_alias) on the layer or a caller merged frame, intersected with the selection","content":"opaque colour or a caller pattern tile (RGBA pixels, 1x1 legal, id authoritative) tiled from its origin, nearest pixel","blend":"blend_math::composite_over with the opacity; behind and clear computed here; group-only / unrecovered modes refuse typed","limits":{"stops":256,"coordinate":1e9,"pattern_side":4096},"parity":"NOT_PROVEN (Photoshop UNVERIFIED, D5: tolerance metric, anti-alias kernel, dither pattern, midpoint curve, radial/angle/diamond drag mapping)"}"#;
