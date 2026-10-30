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
use crate::blend_math::{BlendError, PixelSite, Rgba, StudioBlendMode, blend_rgb, composite_over, site_unit};
use crate::effect_math::style::gradient_sample;
use crate::effect_math::{GradientOverlayOp, GradientStopOp, GradientStyle, MAX_PATTERN_SIDE, PatternOverlayOp, PatternRef};
use crate::render_plan::SourceKey;
use crate::select_tools::wand::{WandOptions, magic_wand};
use crate::select_tools::{ToolError, ToolLimits};
use crate::selection::{ColourView, CombineMode, CoveragePlane, SelectionContext, combine};
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
    check_paint(paint)?;
    match gradient.form {
        GradientForm::Stops => {}
        GradientForm::Noise => return Err(FillError::NoiseUnsupported),
    }
    if gradient.geometry == GradientGeometry::ShapeBurst {
        return Err(FillError::ShapeBurstUnsupported);
    }
    if gradient.smoothness != SMOOTHNESS_FULL {
        return Err(FillError::SmoothnessUnsupported);
    }
    let geometry = Geometry::new(gradient.geometry, drag)?;
    let colours = StopList::colour(gradient.colour_stops)?;
    let alphas = StopList::transparency(gradient.transparency_stops)?;
    check_view(target)?;
    let region = Region::new(target, selection, canvas, limits)?;
    region.paint(target, paint, |x, y| {
        let t = geometry.t(x, y);
        let t = if gradient.reverse { 1.0 - t } else { t };
        let (mut rgb, _) = colours.sample(t);
        let (_, opacity) = alphas.sample(t);
        if gradient.dither {
            for (c, v) in rgb.iter_mut().enumerate() {
                let site = PixelSite { x, y, seed: gradient.dither_seed ^ c as u64 };
                *v = (*v + (site_unit(site) - 0.5) / 255.0).clamp(0.0, 1.0);
            }
        }
        Ok(Rgba { rgb, a: opacity })
    })
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
    check_paint(paint)?;
    let source = Content::new(content)?;
    check_coordinate(seed)?;
    let (sx, sy) = (seed[0].floor() as i64, seed[1].floor() as i64);
    let (cx, cy) = canvas.end()?;
    if sx < canvas.x || sy < canvas.y || sx >= cx || sy >= cy {
        return Err(FillError::SeedOutsideCanvas);
    }
    check_view(target)?;
    let sample = sample.unwrap_or(target);
    check_view(sample)?;
    let wand = WandOptions { tolerance: opts.tolerance, contiguous: opts.contiguous, sample_merged: false, ..WandOptions::default() };
    let ctx = SelectionContext { anti_alias: opts.anti_alias, ..SelectionContext::new(canvas) };
    let mut flood = magic_wand(std::slice::from_ref(sample), 0, (sx, sy), &wand, &ctx, limits)?;
    if let Some(sel) = selection {
        flood = combine(Some(&flood), sel, CombineMode::Intersect, &ctx.limits)?;
    }
    let region = Region::new(target, Some(&flood), canvas, limits)?;
    region.paint(target, paint, |x, y| source.at(x, y))
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
    check_paint(paint)?;
    let source = Content::new(content)?;
    check_view(target)?;
    let region = Region::new(target, selection, canvas, limits)?;
    region.paint(target, paint, |x, y| source.at(x, y))
}

fn unit(v: f32) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}

fn check_paint(paint: &FillPaint) -> Result<(), FillError> {
    if !unit(paint.opacity) {
        return Err(FillError::OpacityOutOfRange);
    }
    match paint.mode {
        StudioBlendMode::Behind | StudioBlendMode::Clear => Ok(()),
        m => blend_rgb(m, [0.0; 3], [0.0; 3]).map(|_| ()).map_err(FillError::from),
    }
}

fn check_coordinate(p: [f64; 2]) -> Result<(), FillError> {
    if !p.iter().all(|v| v.is_finite()) {
        return Err(Error::Nonfinite.into());
    }
    if !p.iter().all(|v| v.abs() <= MAX_COORDINATE) {
        return Err(FillError::CoordinateOutOfRange);
    }
    Ok(())
}

fn check_view(view: &ColourView<'_>) -> Result<(), FillError> {
    let n = usize::try_from(view.bounds.area()).map_err(|_| Error::Overflow)?;
    view.bounds.end()?;
    if n.checked_mul(12) != Some(view.colour.len()) || n.checked_mul(4) != Some(view.alpha.len()) {
        return Err(Error::MalformedStride.into());
    }
    Ok(())
}

fn f32_at(bytes: &[u8], index: usize) -> Option<f32> {
    let at = index.checked_mul(4)?;
    Some(f32::from_le_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

/// The drag-line mapping of one geometry (module doc).
struct Geometry {
    kind: GradientGeometry,
    from: [f64; 2],
    d: [f64; 2],
    len: f64,
    len2: f64,
}
impl Geometry {
    fn new(kind: GradientGeometry, drag: DragLine) -> Result<Self, FillError> {
        check_coordinate(drag.from)?;
        check_coordinate(drag.to)?;
        let d = [drag.to[0] - drag.from[0], drag.to[1] - drag.from[1]];
        let len2 = d[0] * d[0] + d[1] * d[1];
        if len2 <= 0.0 || !len2.is_finite() {
            return Err(FillError::ZeroLengthDrag);
        }
        Ok(Self { kind, from: drag.from, d, len: len2.sqrt(), len2 })
    }
    /// `t` in `[0, 1]` at the centre of pixel `(x, y)`.
    fn t(&self, x: i64, y: i64) -> f64 {
        let v = [x as f64 + 0.5 - self.from[0], y as f64 + 0.5 - self.from[1]];
        let dot = v[0] * self.d[0] + v[1] * self.d[1];
        // Perpendicular component, positive counter-clockwise on screen (y down): n = (dy, -dx).
        let cross = v[0] * self.d[1] - v[1] * self.d[0];
        let t = match self.kind {
            GradientGeometry::Linear => dot / self.len2,
            GradientGeometry::Reflected => dot.abs() / self.len2,
            GradientGeometry::Radial => v[0].hypot(v[1]) / self.len,
            GradientGeometry::Diamond => (dot.abs() + cross.abs()) / self.len2,
            GradientGeometry::Angle => {
                if v[0] == 0.0 && v[1] == 0.0 {
                    0.0
                } else {
                    cross.atan2(dot).rem_euclid(std::f64::consts::TAU) / std::f64::consts::TAU
                }
            }
            GradientGeometry::ShapeBurst => 0.0,
        };
        if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 }
    }
}

/// One stop list: locations, midpoints and per-segment two-stop overlay ops (the overlay's own
/// interpolation, `effect_math::style::gradient_sample`).
struct StopList {
    locations: Vec<f64>,
    midpoints: Vec<f64>,
    ends: (GradientStopOp, GradientStopOp),
    segments: Vec<GradientOverlayOp>,
}
impl StopList {
    fn build(stops: &[(u16, f32, GradientStopOp)]) -> Result<Self, FillError> {
        if stops.is_empty() {
            return Err(FillError::TooFewStops);
        }
        if stops.len() > MAX_FILL_STOPS {
            return Err(FillError::TooManyStops);
        }
        for (location, midpoint, s) in stops {
            if *location > LOCATION_END
                || !(midpoint.is_finite() && (0.0..=100.0).contains(midpoint))
                || !s.colour.iter().all(|c| unit(*c))
                || !unit(s.opacity)
            {
                return Err(FillError::StopOutOfRange);
            }
        }
        if stops.windows(2).any(|w| matches!(w, [a, b] if b.0 < a.0)) {
            return Err(FillError::StopsOutOfOrder);
        }
        let mut segments = Vec::with_capacity(stops.len().saturating_sub(1));
        for w in stops.windows(2) {
            let [(_, _, a), (_, _, b)] = w else { continue };
            let pair = [GradientStopOp { position: 0.0, ..*a }, GradientStopOp { position: 1.0, ..*b }];
            segments.push(GradientOverlayOp::new(StudioBlendMode::Normal, 1.0, GradientStyle::Linear, 0.0, &pair).map_err(|_| FillError::StopOutOfRange)?);
        }
        let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
            return Err(FillError::TooFewStops);
        };
        Ok(Self {
            locations: stops.iter().map(|s| f64::from(s.0) / f64::from(LOCATION_END)).collect(),
            midpoints: stops.iter().map(|s| f64::from(s.1) / 100.0).collect(),
            ends: (first.2, last.2),
            segments,
        })
    }
    fn colour(stops: &[ColourStop]) -> Result<Self, FillError> {
        if stops.len() > MAX_FILL_STOPS {
            return Err(FillError::TooManyStops);
        }
        let list: Vec<_> = stops.iter().map(|s| (s.location, s.midpoint, GradientStopOp { position: 0.0, colour: s.colour, opacity: 1.0 })).collect();
        Self::build(&list)
    }
    fn transparency(stops: &[TransparencyStop]) -> Result<Self, FillError> {
        if stops.len() > MAX_FILL_STOPS {
            return Err(FillError::TooManyStops);
        }
        let mut list = Vec::with_capacity(stops.len());
        for s in stops {
            if !(s.opacity.is_finite() && (0.0..=100.0).contains(&s.opacity)) {
                return Err(FillError::StopOutOfRange);
            }
            list.push((s.location, s.midpoint, GradientStopOp { position: 0.0, colour: [0.0; 3], opacity: s.opacity / 100.0 }));
        }
        Self::build(&list)
    }
    /// Colour and opacity at `t` (module doc).
    fn sample(&self, t: f64) -> ([f32; 3], f32) {
        let (first, last) = self.ends;
        let (Some(lo), Some(hi)) = (self.locations.first(), self.locations.last()) else {
            return (first.colour, first.opacity);
        };
        if t <= *lo {
            return (first.colour, first.opacity);
        }
        if t >= *hi {
            return (last.colour, last.opacity);
        }
        // lo < t < hi: at least one location is <= t and at least one is > t.
        let i = self.locations.partition_point(|l| *l <= t).saturating_sub(1);
        let (Some(a), Some(b), Some(m), Some(op)) =
            (self.locations.get(i), self.locations.get(i.saturating_add(1)), self.midpoints.get(i), self.segments.get(i))
        else {
            return (last.colour, last.opacity);
        };
        let f = ((t - a) / (b - a)).clamp(0.0, 1.0);
        let g = if f < *m {
            0.5 * f / m
        } else if *m >= 1.0 {
            1.0
        } else {
            0.5 + 0.5 * (f - m) / (1.0 - m)
        };
        gradient_sample(op, g)
    }
}

/// Resolved bucket / Edit > Fill content.
enum Content<'a> {
    Colour([f32; 3]),
    Pattern(PatternOverlayOp, &'a [[f32; 4]]),
}
impl<'a> Content<'a> {
    fn new(content: &FillContent<'a>) -> Result<Self, FillError> {
        match content {
            FillContent::Colour(rgb) => {
                if !rgb.iter().all(|c| unit(*c)) {
                    return Err(FillError::Blend(BlendError::OutOfDomain));
                }
                Ok(Self::Colour(*rgb))
            }
            FillContent::Pattern(p) => {
                if p.id.is_empty() {
                    return Err(FillError::PatternIdMissing);
                }
                if p.width == 0 || p.height == 0 {
                    return Err(FillError::PatternEmpty);
                }
                if p.width > MAX_PATTERN_SIDE || p.height > MAX_PATTERN_SIDE {
                    return Err(FillError::PatternTooLarge);
                }
                let n = usize::try_from(p.width)
                    .ok()
                    .zip(usize::try_from(p.height).ok())
                    .and_then(|(w, h)| w.checked_mul(h))
                    .ok_or(Error::Overflow)?;
                if p.pixels.len() != n || !p.pixels.iter().all(|px| px.iter().all(|c| unit(*c))) {
                    return Err(FillError::PatternShape);
                }
                if !p.origin.iter().all(|v| v.unsigned_abs() <= MAX_PATTERN_ORIGIN.unsigned_abs()) {
                    return Err(FillError::PatternOriginOutOfRange);
                }
                let pattern = PatternRef { key: SourceKey(0), width: p.width, height: p.height, colour_sha256: [0; 32], alpha_sha256: [0; 32] };
                let op = PatternOverlayOp::new(StudioBlendMode::Normal, 1.0, pattern)
                    .with_phase([p.origin[0] as f32, p.origin[1] as f32])
                    .with_link(false);
                Ok(Self::Pattern(op, p.pixels))
            }
        }
    }
    fn at(&self, x: i64, y: i64) -> Result<Rgba, FillError> {
        match self {
            Self::Colour(rgb) => Ok(Rgba { rgb: *rgb, a: 1.0 }),
            Self::Pattern(op, pixels) => {
                let unused = Rect { x: 0, y: 0, width: 1, height: 1 };
                let p = op.sample_at(x, y, unused, pixels).ok_or(FillError::PatternShape)?;
                Ok(Rgba { rgb: [p[0], p[1], p[2]], a: p[3] })
            }
        }
    }
}

/// The filled region: `target` x `canvas` x the coverage source, with its coverage plane.
struct Region {
    bounds: Rect,
    coverage: CoveragePlane,
}
impl Region {
    fn new(target: &ColourView<'_>, coverage: Option<&CoveragePlane>, canvas: Rect, limits: &ToolLimits) -> Result<Self, FillError> {
        let mut bounds = target.bounds.intersect(canvas)?.ok_or(FillError::NothingToFill)?;
        if let Some(c) = coverage {
            bounds = bounds.intersect(c.bounds)?.ok_or(FillError::NothingToFill)?;
        }
        if bounds.area() == 0 {
            return Err(FillError::NothingToFill);
        }
        // Replacement colour + alpha + coverage: 18 bytes per pixel, admitted before allocating.
        let n = limits.admit(bounds.area(), 18)?;
        let (ex, ey) = bounds.end()?;
        let mut data = Vec::with_capacity(n);
        for y in bounds.y..ey {
            for x in bounds.x..ex {
                data.push(coverage.map_or(u16::MAX, |c| c.at(x, y)));
            }
        }
        if data.iter().all(|v| *v == 0) {
            return Err(FillError::NothingToFill);
        }
        Ok(Self { bounds, coverage: CoveragePlane { bounds, data } })
    }

    /// Replacement planes: `source(x, y)` composited over the target where covered, the target's
    /// own bytes elsewhere.
    fn paint(self, target: &ColourView<'_>, paint: &FillPaint, source: impl Fn(i64, i64) -> Result<Rgba, FillError>) -> Result<FillResult, FillError> {
        let b = self.bounds;
        let n = self.coverage.data.len();
        let mut colour = Vec::with_capacity(n.checked_mul(12).ok_or(Error::Overflow)?);
        let mut alpha = Vec::with_capacity(n.checked_mul(4).ok_or(Error::Overflow)?);
        let (ex, ey) = b.end()?;
        let tw = target.bounds.width as usize;
        let mut k = 0usize;
        for y in b.y..ey {
            for x in b.x..ex {
                let dx = usize::try_from(x.checked_sub(target.bounds.x).ok_or(Error::Overflow)?).map_err(|_| Error::Overflow)?;
                let dy = usize::try_from(y.checked_sub(target.bounds.y).ok_or(Error::Overflow)?).map_err(|_| Error::Overflow)?;
                let i = dy.checked_mul(tw).and_then(|v| v.checked_add(dx)).ok_or(Error::Overflow)?;
                let cov = self.coverage.data.get(k).copied().unwrap_or(0);
                k = k.checked_add(1).ok_or(Error::Overflow)?;
                let c0 = i.checked_mul(12).ok_or(Error::Overflow)?;
                let a0 = i.checked_mul(4).ok_or(Error::Overflow)?;
                let own_c = target.colour.get(c0..c0.checked_add(12).ok_or(Error::Overflow)?).ok_or(Error::MalformedStride)?;
                let own_a = target.alpha.get(a0..a0.checked_add(4).ok_or(Error::Overflow)?).ok_or(Error::MalformedStride)?;
                if cov == 0 {
                    colour.extend_from_slice(own_c);
                    alpha.extend_from_slice(own_a);
                    continue;
                }
                let at = |j: usize| f32_at(target.colour, i.checked_mul(3)?.checked_add(j)?);
                let (Some(r), Some(g), Some(bl), Some(a)) = (at(0), at(1), at(2), f32_at(target.alpha, i)) else {
                    return Err(Error::MalformedStride.into());
                };
                let backdrop = Rgba { rgb: [r, g, bl], a };
                let src = source(x, y)?;
                let out = mix(paint, backdrop, src, x, y)?;
                for c in out.rgb {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&out.a.to_le_bytes());
            }
        }
        Ok(FillResult { bounds: b, colour, alpha, coverage: self.coverage })
    }
}

/// The paint over one backdrop pixel (module doc: blend).
fn mix(paint: &FillPaint, backdrop: Rgba, source: Rgba, x: i64, y: i64) -> Result<Rgba, FillError> {
    let site = PixelSite { x, y, seed: paint.seed };
    match paint.mode {
        StudioBlendMode::Behind => {
            let below = Rgba { rgb: source.rgb, a: source.a * paint.opacity };
            Ok(composite_over(StudioBlendMode::Normal, below, backdrop, 1.0, site)?)
        }
        StudioBlendMode::Clear => {
            if !(unit(backdrop.a) && backdrop.rgb.iter().all(|c| unit(*c)) && unit(source.a)) {
                return Err(BlendError::OutOfDomain.into());
            }
            Ok(Rgba { rgb: backdrop.rgb, a: backdrop.a * (1.0 - source.a * paint.opacity) })
        }
        m => Ok(composite_over(m, backdrop, source, paint.opacity, site)?),
    }
}

/// Fill-tool surface description (Studio readings, parity NOT_PROVEN).
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"fill_tools","version":1,"ops":["gradient_fill","flood_fill","fill_selection"],"output":"replacement rgb_f32le + straight f32le alpha + coverage plane for masked_replace; no document writes","gradient":"independent colour and transparency stop lists (STU-RAS-160), location 0..4096, midpoint piecewise-linear (f = m gives 0.5), segment interpolation = effect_math gradient_sample; geometries linear, radial, angle, reflected, diamond from a drag line (see module doc); reverse; dither seeded per site (+-0.5/255)","refused":["form noise: gradient_noise_unsupported","geometry shape_burst: gradient_shape_burst_unsupported","interpolation != 4096: gradient_smoothness_unsupported"],"flood":"region = select_tools::wand::magic_wand (tolerance, contiguous, anti_alias) on the layer or a caller merged frame, intersected with the selection","content":"opaque colour or a caller pattern tile (RGBA pixels, 1x1 legal, id authoritative) tiled from its origin, nearest pixel","blend":"blend_math::composite_over with the opacity; behind and clear computed here; group-only / unrecovered modes refuse typed","limits":{"stops":256,"coordinate":1e9,"pattern_side":4096},"parity":"NOT_PROVEN (Photoshop UNVERIFIED, D5: tolerance metric, anti-alias kernel, dither pattern, midpoint curve, radial/angle/diamond drag mapping)"}"#;
