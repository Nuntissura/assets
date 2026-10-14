//! Retouch family (lane SMB-PIGMENT-RETOUCH, Photoshop parity gap PS-16; spec basis STU-RAS-026
//! retouching family, STU-RAS-139 clone-stamp option contract): clone stamp, healing brush and
//! spot healing over straight-RGBA planes, driven by the brush engine's dabs ([`crate::brush`])
//! and clipped by selection planes ([`crate::selection`]). Pure compute: no ports, no leases, no
//! Folio writes; the caller publishes the returned plane (for example through `masked_replace`).
//!
//! Clone stamp. A [`CloneSource`] holds the sampled source point `S` and the options. Each stroke
//! begins with [`CloneSource::begin_stroke`] at its first dab `D` and returns a [`CloneMap`]: the
//! affine map SAMPLE-plane coordinates -> DESTINATION coordinates, `d = R(theta) (s - S) + A`,
//! with the anchor `A` = the first dab of the FIRST stroke when `aligned` (one fixed offset for
//! every later stroke) and the first dab of EACH stroke when not aligned (every stroke restarts
//! at `S`). The sampled pixel of destination pixel `(x, y)` is read through
//! [`crate::transform_math::PreparedTransform::sample_rgba`] (the layer-transform reference
//! sampler; a destination centre landing on a sample texel centre returns that texel unchanged,
//! so an integer offset with no rotation is an exact pixel copy). Painting composites the sampled
//! pixel source-over the destination with its alpha scaled by the stroke coverage
//! (`brush::stroke_coverage`, intersected with the selection), in binary64, rounded once to
//! binary32; pixels with zero coverage keep their bytes. The source-over application, the anchor
//! rule and the use of the sampler's bilinear filter for fractional offsets are Studio readings of
//! Adobe help text, class Approximate, UNVERIFIED against Photoshop (oracle list D5).
//!
//! Sample scope (`current_layer | current_and_below | all_layers`) and cross-document sources are
//! resolved by the caller: the operator takes the resolved sample plane.
//!
//! Hostile inputs (V4-01/02): every plane sample must be finite and in `[0, 1]`; plane lengths must
//! match their rectangles; output bytes and work are checked against [`RetouchLimits`] before any
//! allocation; integer arithmetic is checked.
#![deny(clippy::arithmetic_side_effects)]
use crate::blend_math::Exactness;
use crate::brush::{BrushError, BrushLimits, Dab, clip_to_selection, stroke_coverage};
use crate::selection::{CoveragePlane, SelectionError};
use crate::transform_math::{
    Affine2, EdgeMode, LayerTransform, PreparedTransform, ResampleFilter, TransformError,
};
use crate::{Error, MAX_BYTES, Rect};
use std::fmt;

/// Class of every retouch operator (Studio readings; Photoshop parity UNVERIFIED, D5).
pub const RETOUCH_EXACTNESS: Exactness = Exactness::Approximate;
/// Bytes of one straight-RGBA sample (`[f32; 4]`).
pub const RGBA_SAMPLE_BYTES: u64 = 16;

/// Typed failure of a retouch operator.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetouchError {
    Pigment(Error),
    Brush(BrushError),
    Selection(SelectionError),
    Transform(TransformError),
    /// An option is outside its contract range (never clamped).
    OutOfRange,
    /// A plane sample is non-finite or outside `[0, 1]`.
    OutOfDomain,
    /// A plane length disagrees with its rectangle.
    Shape,
    /// Spot healing found no candidate source region fully inside the sample plane.
    SpotHealNoSource,
    /// Content-aware fill / move / patch / inpaint are not implemented (STU-RAS-143): refused,
    /// never approximated by another fill.
    ContentAwareUnsupported,
    /// A rotated crop with `delete_cropped_pixels = false` (canvas_ops): not representable.
    CropRotateKeepPixelsUnsupported,
    /// A trim whose basis matches every pixel (canvas_ops).
    TrimEverything,
}
impl RetouchError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Pigment(e) => e.code(),
            Self::Brush(e) => e.code(),
            Self::Selection(e) => e.code(),
            Self::Transform(e) => e.code(),
            Self::OutOfRange => "retouch_out_of_range",
            Self::OutOfDomain => "retouch_out_of_domain",
            Self::Shape => "retouch_shape",
            Self::SpotHealNoSource => "spot_heal_no_source",
            Self::ContentAwareUnsupported => "content_aware_unsupported",
            Self::CropRotateKeepPixelsUnsupported => "crop_rotate_keep_pixels_unsupported",
            Self::TrimEverything => "trim_everything",
        }
    }
}
impl From<Error> for RetouchError {
    fn from(e: Error) -> Self {
        Self::Pigment(e)
    }
}
impl From<BrushError> for RetouchError {
    fn from(e: BrushError) -> Self {
        Self::Brush(e)
    }
}
impl From<SelectionError> for RetouchError {
    fn from(e: SelectionError) -> Self {
        Self::Selection(e)
    }
}
impl From<TransformError> for RetouchError {
    fn from(e: TransformError) -> Self {
        Self::Transform(e)
    }
}
impl fmt::Display for RetouchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for RetouchError {}

/// Budgets checked before allocation (V4-02): owned output bytes and work (sample visits or solver
/// cell updates).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetouchLimits {
    pub max_bytes: u64,
    pub max_work: u64,
}
impl Default for RetouchLimits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_BYTES,
            max_work: 1 << 32,
        }
    }
}
impl RetouchLimits {
    pub(crate) fn brush(&self) -> BrushLimits {
        BrushLimits {
            max_bytes: self.max_bytes,
            ..BrushLimits::default()
        }
    }
    /// Refuse `area` samples of `bytes_per_sample` above the byte budget; returns the count.
    pub fn admit_area(&self, r: Rect, bytes_per_sample: u64) -> Result<usize, Error> {
        let bytes = r.area().checked_mul(bytes_per_sample).ok_or(Error::Overflow)?;
        if bytes > self.max_bytes {
            return Err(Error::BudgetExceeded);
        }
        usize::try_from(r.area()).map_err(|_| Error::Overflow)
    }
    pub fn admit_work(&self, work: u64) -> Result<(), Error> {
        if work > self.max_work {
            Err(Error::BudgetExceeded)
        } else {
            Ok(())
        }
    }
}

/// Straight RGBA plane: row-major over `bounds`, every component finite and in `[0, 1]`;
/// transparent outside `bounds`.
#[derive(Clone, Debug, PartialEq)]
pub struct RgbaPlane {
    pub bounds: Rect,
    pub data: Vec<[f32; 4]>,
}
impl RgbaPlane {
    /// Validated plane (length = area, every sample finite and in `[0, 1]`).
    pub fn new(bounds: Rect, data: Vec<[f32; 4]>) -> Result<Self, RetouchError> {
        let plane = Self { bounds, data };
        plane.validate()?;
        Ok(plane)
    }
    /// A plane of one colour; the byte budget is checked before allocating.
    pub fn filled(bounds: Rect, rgba: [f32; 4], limits: &RetouchLimits) -> Result<Self, RetouchError> {
        check_sample(rgba)?;
        bounds.end()?;
        let n = limits.admit_area(bounds, RGBA_SAMPLE_BYTES)?;
        Ok(Self {
            bounds,
            data: vec![rgba; n],
        })
    }
    pub fn validate(&self) -> Result<(), RetouchError> {
        self.bounds.end()?;
        let n = usize::try_from(self.bounds.area()).map_err(|_| Error::Overflow)?;
        if self.data.len() != n {
            return Err(RetouchError::Shape);
        }
        for p in &self.data {
            check_sample(*p)?;
        }
        Ok(())
    }
    /// Index of plan pixel `(x, y)`, or `None` outside the bounds.
    pub fn index(&self, x: i64, y: i64) -> Option<usize> {
        let dx = u32::try_from(x.checked_sub(self.bounds.x)?).ok()?;
        let dy = u32::try_from(y.checked_sub(self.bounds.y)?).ok()?;
        if dx >= self.bounds.width || dy >= self.bounds.height {
            return None;
        }
        (dy as usize)
            .checked_mul(self.bounds.width as usize)?
            .checked_add(dx as usize)
    }
    /// The sample at `(x, y)`; transparent outside the bounds.
    pub fn at(&self, x: i64, y: i64) -> [f32; 4] {
        self.index(x, y)
            .and_then(|i| self.data.get(i))
            .copied()
            .unwrap_or([0.0; 4])
    }
    /// The texel at `(column, row)` of the plane (no bounds offset).
    pub(crate) fn texel(&self, column: u32, row: u32) -> [f32; 4] {
        (row as usize)
            .checked_mul(self.bounds.width as usize)
            .and_then(|r| r.checked_add(column as usize))
            .and_then(|i| self.data.get(i))
            .copied()
            .unwrap_or([0.0; 4])
    }
}

pub(crate) fn check_sample(p: [f32; 4]) -> Result<(), RetouchError> {
    if p.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
        Ok(())
    } else {
        Err(RetouchError::OutOfDomain)
    }
}

/// STU-RAS-139 `sample_scope`: which layers the caller flattens into the sample plane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SampleScope {
    #[default]
    CurrentLayer,
    CurrentAndBelow,
    AllLayers,
}
impl SampleScope {
    pub const fn key(self) -> &'static str {
        match self {
            Self::CurrentLayer => "current_layer",
            Self::CurrentAndBelow => "current_and_below",
            Self::AllLayers => "all_layers",
        }
    }
}

/// STU-RAS-139 clone-stamp options (defaults: aligned true, rotation 0, scope current layer).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloneOptions {
    pub aligned: bool,
    /// Degrees, `-180 ..= 180`.
    pub source_rotation_deg: f64,
    pub sample_scope: SampleScope,
    /// Filter for a source position between texel centres (fractional offset or rotation).
    pub filter: ResampleFilter,
}
impl Default for CloneOptions {
    fn default() -> Self {
        Self {
            aligned: true,
            source_rotation_deg: 0.0,
            sample_scope: SampleScope::CurrentLayer,
            filter: ResampleFilter::Bilinear,
        }
    }
}

/// The sampled source point and the stroke anchor state of a clone session.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloneSource {
    source: (f64, f64),
    options: CloneOptions,
    anchor: Option<(f64, f64)>,
}
impl CloneSource {
    /// Source point in sample-plane pixels (continuous coordinates).
    pub fn new(x: f64, y: f64, options: CloneOptions) -> Result<Self, RetouchError> {
        if !(x.is_finite() && y.is_finite()) {
            return Err(Error::Nonfinite.into());
        }
        let r = options.source_rotation_deg;
        if !(r.is_finite() && (-180.0..=180.0).contains(&r)) {
            return Err(RetouchError::OutOfRange);
        }
        Ok(Self {
            source: (x, y),
            options,
            anchor: None,
        })
    }
    pub fn options(&self) -> CloneOptions {
        self.options
    }
    /// Start a stroke whose first dab is `first`: returns the stroke's sample -> destination map.
    pub fn begin_stroke(&mut self, first: &Dab) -> Result<CloneMap, RetouchError> {
        if !(first.x.is_finite() && first.y.is_finite()) {
            return Err(Error::Nonfinite.into());
        }
        let anchor = match (self.options.aligned, self.anchor) {
            (true, Some(a)) => a,
            _ => (first.x, first.y),
        };
        self.anchor = Some(anchor);
        let (sx, sy) = self.source;
        let affine = Affine2::from_parts(
            sx,
            sy,
            1.0,
            1.0,
            self.options.source_rotation_deg,
            anchor.0 - sx,
            anchor.1 - sy,
        );
        CloneMap::new(affine, self.options.filter)
    }
}

/// Sample-plane -> destination affine map of one clone stroke, with its filter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloneMap {
    affine: Affine2,
    filter: ResampleFilter,
}
impl CloneMap {
    /// Validated map (finite, invertible, bounded like a layer transform).
    pub fn new(affine: Affine2, filter: ResampleFilter) -> Result<Self, RetouchError> {
        affine.inverse()?;
        Ok(Self { affine, filter })
    }
    /// A pure offset: destination = sample + `(dx, dy)`.
    pub fn offset(dx: f64, dy: f64, filter: ResampleFilter) -> Result<Self, RetouchError> {
        Self::new(Affine2::translate(dx, dy), filter)
    }
    pub fn affine(&self) -> Affine2 {
        self.affine
    }
    /// The sampled (straight RGBA) pixels of `sample` for every destination pixel of `region`.
    pub fn sample(
        &self,
        sample: &RgbaPlane,
        region: Rect,
        limits: &RetouchLimits,
    ) -> Result<RgbaPlane, RetouchError> {
        region.end()?;
        let n = limits.admit_area(region, RGBA_SAMPLE_BYTES)?;
        if sample.bounds.width == 0 || sample.bounds.height == 0 || n == 0 {
            return RgbaPlane::filled(region, [0.0; 4], limits);
        }
        let prepared = PreparedTransform::new(
            &LayerTransform::new(self.affine, self.filter).with_edge(EdgeMode::Transparent),
            sample.bounds,
        )?;
        let (ex, ey) = region.end()?;
        let mut data = Vec::with_capacity(n);
        for y in region.y..ey {
            for x in region.x..ex {
                let p = prepared
                    .sample_rgba(x, y, |i, j| Ok::<_, RetouchError>(sample.texel(i, j)))?;
                data.push(p);
            }
        }
        Ok(RgbaPlane {
            bounds: region,
            data,
        })
    }
}

/// The painted footprint of one stroke: its dabs, the tip hardness and an optional selection the
/// stroke is clipped to.
#[derive(Clone, Copy, Debug)]
pub struct StrokeFootprint<'a> {
    pub dabs: &'a [Dab],
    pub hardness_pct: f64,
    pub selection: Option<&'a CoveragePlane>,
}

/// Stroke coverage over `canvas`, intersected with the selection when present.
pub fn footprint_coverage(
    canvas: Rect,
    footprint: &StrokeFootprint<'_>,
    limits: &RetouchLimits,
) -> Result<CoveragePlane, RetouchError> {
    let brush = limits.brush();
    let stroke = stroke_coverage(footprint.dabs, footprint.hardness_pct, canvas, &brush)?;
    match footprint.selection {
        Some(sel) => Ok(clip_to_selection(&stroke, sel, &brush)?),
        None => Ok(stroke),
    }
}

/// Result of a retouch stroke: the whole new target plane and the rectangle that may differ.
#[derive(Clone, Debug, PartialEq)]
pub struct RetouchResult {
    pub plane: RgbaPlane,
    /// `None` when nothing was covered.
    pub damage: Option<Rect>,
}

/// Source-over of straight `src` (alpha scaled by `c`) onto straight `dst`, binary64, one binary32
/// rounding; `c <= 0` or a transparent result keeps `dst`.
pub(crate) fn paint_over(dst: [f32; 4], src: [f32; 4], c: f64) -> [f32; 4] {
    if c <= 0.0 {
        return dst;
    }
    let sa = f64::from(src[3]) * c;
    let da = f64::from(dst[3]);
    let oa = sa + da * (1.0 - sa);
    if oa <= 0.0 {
        return dst;
    }
    let mut out = [0.0_f32; 4];
    for (k, o) in out.iter_mut().take(3).enumerate() {
        let v = (f64::from(src[k]) * sa + f64::from(dst[k]) * da * (1.0 - sa)) / oa;
        *o = v.clamp(0.0, 1.0) as f32;
    }
    out[3] = oa.min(1.0) as f32;
    out
}

/// Copy `target` into a new plane of the same bounds (budget checked).
pub(crate) fn copy_plane(target: &RgbaPlane, limits: &RetouchLimits) -> Result<RgbaPlane, RetouchError> {
    target.validate()?;
    limits.admit_area(target.bounds, RGBA_SAMPLE_BYTES)?;
    Ok(target.clone())
}

/// Clone stamp: paint `map`'s sampled pixels of `sample` onto `target` under the stroke footprint.
pub fn clone_stamp(
    target: &RgbaPlane,
    sample: &RgbaPlane,
    map: &CloneMap,
    footprint: &StrokeFootprint<'_>,
    limits: &RetouchLimits,
) -> Result<RetouchResult, RetouchError> {
    sample.validate()?;
    let mut out = copy_plane(target, limits)?;
    let cov = footprint_coverage(target.bounds, footprint, limits)?;
    if cov.bounds.area() == 0 {
        return Ok(RetouchResult {
            plane: out,
            damage: None,
        });
    }
    let sampled = map.sample(sample, cov.bounds, limits)?;
    let (ex, ey) = cov.bounds.end()?;
    let mut k = 0usize;
    for y in cov.bounds.y..ey {
        for x in cov.bounds.x..ex {
            let c = f64::from(cov.data.get(k).copied().unwrap_or(0)) / f64::from(u16::MAX);
            let s = sampled.data.get(k).copied().unwrap_or([0.0; 4]);
            k = k.checked_add(1).ok_or(Error::Overflow)?;
            if let Some(i) = out.index(x, y)
                && let Some(px) = out.data.get_mut(i)
            {
                *px = paint_over(*px, s, c);
            }
        }
    }
    Ok(RetouchResult {
        plane: out,
        damage: Some(cov.bounds),
    })
}

/// Over-relaxation factor of the healing solver (successive over-relaxation, row-major order).
pub const HEAL_SOR_OMEGA: f64 = 1.9;
/// The solver stops when the largest update of a sweep is at most this (premultiplied units).
pub const HEAL_TOLERANCE: f64 = 1.0e-9;
/// Working bytes per pixel of the healing region (destination, guide, correction in binary64 RGBA
/// plus the domain flag and coverage), charged before allocation.
pub const HEAL_BYTES_PER_PIXEL: u64 = 112;

/// How a healing solve ended (never a silent partial result: `converged` is reported).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HealReceipt {
    /// Sweeps run.
    pub iterations: u32,
    /// The largest update of the last sweep fell to [`HEAL_TOLERANCE`].
    pub converged: bool,
    /// Largest update of the last sweep.
    pub residual: f64,
    /// Spot healing: the source displacement chosen (destination pixel `p` reads `p + offset`).
    pub source_offset: Option<(i64, i64)>,
}

/// Result of a healing stroke.
#[derive(Clone, Debug, PartialEq)]
pub struct HealResult {
    pub result: RetouchResult,
    pub receipt: HealReceipt,
}

fn premul(p: [f32; 4]) -> [f64; 4] {
    let a = f64::from(p[3]);
    [f64::from(p[0]) * a, f64::from(p[1]) * a, f64::from(p[2]) * a, a]
}

fn straight(p: [f64; 4]) -> [f32; 4] {
    let a = p[3].clamp(0.0, 1.0);
    if a <= 0.0 || a.is_nan() {
        return [0.0; 4];
    }
    let c = |v: f64| {
        let q = v.clamp(0.0, a) / a;
        if q.is_nan() { 0.0 } else { q.clamp(0.0, 1.0) as f32 }
    };
    [c(p[0]), c(p[1]), c(p[2]), a as f32]
}

/// `r` grown by `g` pixels on every side.
fn grow(r: Rect, g: u32) -> Result<Rect, Error> {
    let gi = i64::from(g);
    let twice = g.checked_mul(2).ok_or(Error::Overflow)?;
    let grown = Rect {
        x: r.x.checked_sub(gi).ok_or(Error::Overflow)?,
        y: r.y.checked_sub(gi).ok_or(Error::Overflow)?,
        width: r.width.checked_add(twice).ok_or(Error::Overflow)?,
        height: r.height.checked_add(twice).ok_or(Error::Overflow)?,
    };
    grown.end()?;
    Ok(grown)
}

/// `r` grown by `g` pixels on every side, intersected with `within`.
fn grow_within(r: Rect, g: u32, within: Rect) -> Result<Option<Rect>, Error> {
    grow(r, g)?.intersect(within)
}

/// Column and row of index `i` in a row-major grid of width `w` (> 0).
fn col_row(i: usize, w: usize) -> Result<(usize, usize), Error> {
    Ok((
        i.checked_rem(w).ok_or(Error::Overflow)?,
        i.checked_div(w).ok_or(Error::Overflow)?,
    ))
}

/// Healing (gradient-domain) blend over the stroke footprint. With `g` the cloned source and `d`
/// the destination (premultiplied RGBA, binary64), the healed value inside the domain
/// `Omega = {coverage > 0}` is `f = g + h`, where `h` solves Laplace's equation `lap(h) = 0` in
/// `Omega` with the Dirichlet boundary `h = d - g` on the pixels next to `Omega` (4-neighbourhood)
/// and a Neumann (skipped) neighbour beyond the target bounds. `f` therefore has the Laplacian of
/// the source (its texture) and matches the destination's colour and lighting at the boundary:
/// the membrane form of Poisson image editing (Perez, Gangnet, Blake 2003, guidance field = source
/// gradient). Solved by successive over-relaxation ([`HEAL_SOR_OMEGA`], row-major sweeps, one
/// thread) until the largest update is at most [`HEAL_TOLERANCE`] or the work budget ends; the
/// receipt reports which. Output per pixel: premultiplied `d + c (f - d)` with the coverage `c`,
/// alpha clamped to `[0, 1]`, colour to `[0, alpha]`, un-premultiplied and rounded once to
/// binary32. Whether Photoshop's healing brush solves this exact equation is UNVERIFIED (D5): a
/// Studio approximation, class Approximate.
pub fn heal(
    target: &RgbaPlane,
    sample: &RgbaPlane,
    map: &CloneMap,
    footprint: &StrokeFootprint<'_>,
    limits: &RetouchLimits,
) -> Result<HealResult, RetouchError> {
    sample.validate()?;
    let cov = footprint_coverage(target.bounds, footprint, limits)?;
    heal_with_coverage(target, sample, map, &cov, None, limits)
}

fn heal_with_coverage(
    target: &RgbaPlane,
    sample: &RgbaPlane,
    map: &CloneMap,
    cov: &CoveragePlane,
    source_offset: Option<(i64, i64)>,
    limits: &RetouchLimits,
) -> Result<HealResult, RetouchError> {
    let mut out = copy_plane(target, limits)?;
    let unchanged = |plane: RgbaPlane| HealResult {
        result: RetouchResult { plane, damage: None },
        receipt: HealReceipt { iterations: 0, converged: true, residual: 0.0, source_offset },
    };
    if cov.bounds.area() == 0 || cov.data.iter().all(|c| *c == 0) {
        return Ok(unchanged(out));
    }
    let Some(region) = grow_within(cov.bounds, 1, target.bounds)? else {
        return Ok(unchanged(out));
    };
    let n = limits.admit_area(region, HEAL_BYTES_PER_PIXEL)?;
    let guide = map.sample(sample, region, limits)?;
    let rw = region.width as usize;
    let (ex, ey) = region.end()?;
    let mut d = Vec::with_capacity(n);
    let mut g = Vec::with_capacity(n);
    let mut c = Vec::with_capacity(n);
    for y in region.y..ey {
        for x in region.x..ex {
            d.push(premul(target.at(x, y)));
            c.push(f64::from(cov.at(x, y)) / f64::from(u16::MAX));
        }
    }
    for p in &guide.data {
        g.push(premul(*p));
    }
    // h: Dirichlet values outside Omega, unknowns (start 0) inside.
    let mut h: Vec<[f64; 4]> = Vec::with_capacity(n);
    let mut domain: Vec<usize> = Vec::new();
    for i in 0..n {
        let (di, gi) = (d.get(i).copied().unwrap_or([0.0; 4]), g.get(i).copied().unwrap_or([0.0; 4]));
        if c.get(i).copied().unwrap_or(0.0) > 0.0 {
            h.push([0.0; 4]);
            domain.push(i);
        } else {
            h.push([di[0] - gi[0], di[1] - gi[1], di[2] - gi[2], di[3] - gi[3]]);
        }
    }
    // Iteration budget: the work budget over the domain size, capped by a size-derived maximum.
    let cells = u64::try_from(domain.len()).map_err(|_| Error::Overflow)?.max(1);
    let size_cap = u64::from(region.width)
        .checked_add(u64::from(region.height))
        .and_then(|s| s.checked_mul(16))
        .and_then(|s| s.checked_add(256))
        .ok_or(Error::Overflow)?;
    let max_iter = limits
        .max_work
        .checked_div(cells)
        .ok_or(Error::Overflow)?
        .min(size_cap)
        .min(u64::from(u32::MAX));
    if max_iter == 0 {
        return Err(Error::BudgetExceeded.into());
    }
    let rh = region.height as usize;
    let mut iterations = 0u32;
    let mut residual = f64::INFINITY;
    while u64::from(iterations) < max_iter {
        residual = 0.0;
        for &i in &domain {
            let (col, row) = col_row(i, rw)?;
            let mut sum = [0.0f64; 4];
            let mut k = 0u32;
            let nbs = [
                (col > 0).then(|| i.checked_sub(1)).flatten(),
                (col.checked_add(1).is_some_and(|v| v < rw)).then(|| i.checked_add(1)).flatten(),
                (row > 0).then(|| i.checked_sub(rw)).flatten(),
                (row.checked_add(1).is_some_and(|v| v < rh)).then(|| i.checked_add(rw)).flatten(),
            ];
            for nb in nbs.into_iter().flatten() {
                if let Some(v) = h.get(nb) {
                    for (s, x) in sum.iter_mut().zip(v) {
                        *s += *x;
                    }
                    k = k.saturating_add(1);
                }
            }
            if k == 0 {
                continue;
            }
            let kf = f64::from(k);
            if let Some(hi) = h.get_mut(i) {
                for (ch, s) in hi.iter_mut().zip(sum) {
                    let delta = HEAL_SOR_OMEGA * (s / kf - *ch);
                    *ch += delta;
                    residual = residual.max(delta.abs());
                }
            }
        }
        iterations = iterations.saturating_add(1);
        if residual <= HEAL_TOLERANCE {
            break;
        }
    }
    for &i in &domain {
        let (di, gi, hi) = (
            d.get(i).copied().unwrap_or([0.0; 4]),
            g.get(i).copied().unwrap_or([0.0; 4]),
            h.get(i).copied().unwrap_or([0.0; 4]),
        );
        let ci = c.get(i).copied().unwrap_or(0.0);
        let mut v = [0.0f64; 4];
        for k in 0..4 {
            let f = gi[k] + hi[k];
            v[k] = di[k] + ci * (f - di[k]);
        }
        let (col, row) = col_row(i, rw)?;
        let col = i64::try_from(col).map_err(|_| Error::Overflow)?;
        let row = i64::try_from(row).map_err(|_| Error::Overflow)?;
        let (x, y) = (
            region.x.checked_add(col).ok_or(Error::Overflow)?,
            region.y.checked_add(row).ok_or(Error::Overflow)?,
        );
        if let Some(idx) = out.index(x, y)
            && let Some(px) = out.data.get_mut(idx)
        {
            *px = straight(v);
        }
    }
    Ok(HealResult {
        result: RetouchResult {
            plane: out,
            damage: Some(cov.bounds),
        },
        receipt: HealReceipt {
            iterations,
            converged: residual <= HEAL_TOLERANCE,
            residual,
            source_offset,
        },
    })
}

/// Ring width (pixels) around the stroke used to score spot-healing source candidates.
pub const SPOT_RING_PX: u32 = 2;

/// Spot healing (no source point): the source is chosen automatically by proximity match, then
/// healed like [`heal`]. Candidates are displacements of the footprint bounds `B` (`w x h`) by
/// `m (w + 2 ring + 1)` horizontally and `m (h + 2 ring + 1)` vertically for `m = 1, 2` in the
/// fixed direction order E, W, S, N, SE, SW, NE, NW; a candidate whose `B` grown by
/// [`SPOT_RING_PX`] does not lie inside the sample plane is skipped. Score = sum over the ring
/// pixels (inside `B` grown by the ring and inside the target, coverage 0) of the squared
/// premultiplied RGBA difference between the target pixel and the candidate source pixel; the
/// lowest score wins, ties go to the earlier candidate. No candidate: typed
/// [`RetouchError::SpotHealNoSource`]. Photoshop's spot healing ("Proximity Match" type) is the
/// behavioural reference; its candidate search is UNVERIFIED (D5), class Approximate. The
/// "Content-Aware" type is out of scope ([`content_aware_fill`]).
pub fn spot_heal(
    target: &RgbaPlane,
    sample: &RgbaPlane,
    footprint: &StrokeFootprint<'_>,
    limits: &RetouchLimits,
) -> Result<HealResult, RetouchError> {
    sample.validate()?;
    target.validate()?;
    let cov = footprint_coverage(target.bounds, footprint, limits)?;
    if cov.bounds.area() == 0 || cov.data.iter().all(|c| *c == 0) {
        let plane = copy_plane(target, limits)?;
        return Ok(HealResult {
            result: RetouchResult { plane, damage: None },
            receipt: HealReceipt { iterations: 0, converged: true, residual: 0.0, source_offset: None },
        });
    }
    let b = cov.bounds;
    let ring = grow_within(b, SPOT_RING_PX, target.bounds)?.ok_or(RetouchError::SpotHealNoSource)?;
    let ring_full = grow(b, SPOT_RING_PX)?;
    let twice_ring = SPOT_RING_PX.checked_mul(2).ok_or(Error::Overflow)?;
    let step_x = i64::from(b.width.checked_add(twice_ring).and_then(|v| v.checked_add(1)).ok_or(Error::Overflow)?);
    let step_y = i64::from(b.height.checked_add(twice_ring).and_then(|v| v.checked_add(1)).ok_or(Error::Overflow)?);
    let dirs: [(i64, i64); 8] = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (-1, 1), (1, -1), (-1, -1)];
    let ring_cells = ring.area();
    limits.admit_work(ring_cells.checked_mul(16).ok_or(Error::Overflow)?)?;
    let (rx1, ry1) = ring.end()?;
    let mut best: Option<((i64, i64), f64)> = None;
    for m in [1i64, 2] {
        for (dx, dy) in dirs {
            let ox = dx.checked_mul(m).and_then(|v| v.checked_mul(step_x)).ok_or(Error::Overflow)?;
            let oy = dy.checked_mul(m).and_then(|v| v.checked_mul(step_y)).ok_or(Error::Overflow)?;
            let cand = Rect {
                x: ring_full.x.checked_add(ox).ok_or(Error::Overflow)?,
                y: ring_full.y.checked_add(oy).ok_or(Error::Overflow)?,
                width: ring_full.width,
                height: ring_full.height,
            };
            if !sample.bounds.contains(cand)? {
                continue;
            }
            let mut score = 0.0f64;
            for y in ring.y..ry1 {
                for x in ring.x..rx1 {
                    if cov.at(x, y) > 0 {
                        continue;
                    }
                    let t = premul(target.at(x, y));
                    let s = premul(sample.at(
                        x.checked_add(ox).ok_or(Error::Overflow)?,
                        y.checked_add(oy).ok_or(Error::Overflow)?,
                    ));
                    for k in 0..4 {
                        let e = t[k] - s[k];
                        score += e * e;
                    }
                }
            }
            if best.is_none_or(|(_, s)| score < s) {
                best = Some(((ox, oy), score));
            }
        }
    }
    let ((ox, oy), _) = best.ok_or(RetouchError::SpotHealNoSource)?;
    // Destination p reads source p + o: destination = sample - o.
    let map = CloneMap::offset(-(ox as f64), -(oy as f64), ResampleFilter::NearestNeighbour)?;
    heal_with_coverage(target, sample, &map, &cov, Some((ox, oy)), limits)
}

/// Content-aware fill / move / patch / inpaint (STU-RAS-143): not implemented in this lane; always
/// the typed refusal [`RetouchError::ContentAwareUnsupported`] (never another fill in its place).
pub fn content_aware_fill(
    _target: &RgbaPlane,
    _region: &CoveragePlane,
) -> Result<RetouchResult, RetouchError> {
    Err(RetouchError::ContentAwareUnsupported)
}

/// Retouch surface description (Studio readings, parity NOT_PROVEN).
pub const DESCRIPTOR: &str = r#"{"module":"STUDIO-MODULE-PIGMENT","surface":"retouch","version":1,"plane":"RgbaPlane{bounds, straight RGBA f32 in [0,1]}","clone_stamp":"CloneSource(source point, CloneOptions{aligned default true, source_rotation_deg -180..180 default 0, sample_scope current_layer|current_and_below|all_layers resolved by the caller, filter}) -> begin_stroke(first dab) -> CloneMap d = R(theta)(s - S) + A (A = first dab of the first stroke when aligned, of each stroke otherwise); sampled through transform_math PreparedTransform::sample_rgba (integer offset, no rotation = exact copy); painted source-over with alpha * stroke coverage (brush::stroke_coverage intersect selection), binary64, one binary32 rounding; zero coverage keeps bytes","heal":"heal(target, sample, CloneMap, footprint): membrane Poisson blend f = g + h, lap(h) = 0 in coverage>0 domain, h = d - g on its 4-neighbour boundary, Neumann beyond the target; SOR omega 1.9 row-major single thread, tolerance 1e-9, iteration cap min(max_work/domain, 16(w+h)+256); HealReceipt{iterations, converged, residual}; output d + c(f - d) premultiplied","spot_heal":"proximity match: 16 candidate displacements (m=1,2 x E,W,S,N,SE,SW,NE,NW, step = footprint size + 2 ring + 1), candidates fully inside the sample plane, score = squared premultiplied difference over the 2 px ring outside the footprint, lowest wins (ties: earlier), then heal; none = spot_heal_no_source","content_aware":"content_aware_fill refuses content_aware_unsupported (STU-RAS-143 gap)","class":"approximate","parity":"NOT_PROVEN (Photoshop clone/heal behaviour UNVERIFIED, D5)"}"#;
