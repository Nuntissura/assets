//! Layer blur and backdrop blur (lane SMB-COMPOSITE-NEW-KINDS slice 1; Folio
//! `StudioEffectKind::Blur { render_scope: layer | below, radius }`, spec basis STU-FX-002
//! render_scope, STU-FX-013 Gaussian Blur and Background Blur). Single owner (CX-CAP-001) of the
//! blur maths: render-cpu calls these functions, render-gpu transcribes them to WGSL and is
//! tolerance-tested against them.
//!
//! Radius convention: a blur radius `r` in pixels is the Gaussian of [`feather_kernel`]`(r)`, i.e.
//! `sigma = r / 2` with the support `ceil(3 sigma)` taps per side, normalised to sum 1: the SAME
//! convention as the mask feather and the effect matte blurs of `effect_math` (one Studio rule for
//! every Gaussian). That Figma's layer/background blur radius means `sigma = r / 2` (its CSS export
//! writes `blur(r / 2)`) and Photoshop's Gaussian Blur radius relation are UNVERIFIED (D5), so the
//! operators are class Approximate ([`BLUR_EXACTNESS`]).
//!
//! Semantics (Studio reading, UNVERIFIED against Figma/Photoshop):
//! - Layer blur ([`BlurScope::Layer`]): the layer's shape content (straight RGBA, the layer mask
//!   already in its alpha, transparent beyond the layer) is blurred PREMULTIPLIED by the separable
//!   kernel (horizontal then vertical, each pass summed in binary64 in tap order and rounded once
//!   to binary32), un-premultiplied (`A <= 0` is transparent, colour clamped to `[0, 1]`), then
//!   composited with the layer's mode, opacity and fill like the unblurred content.
//! - Backdrop blur ([`BlurScope::Backdrop`]): the group's running result below the layer (the
//!   backdrop, transparent beyond the group's isolated buffer) is blurred the same way, and inside
//!   the layer's shape it is replaced by the blurred backdrop with weight
//!   `k = shape alpha * layer opacity` ([`backdrop_mix`], a premultiplied lerp); the layer content
//!   then composites unchanged on top. Defined only in an isolated group (the backdrop must be the
//!   group's own result).
//!
//! Hostile sizes (V4-01/02): the radius is bounded by [`MAX_BLUR_RADIUS_PX`]; [`blur_window`]
//! computes its bytes and work with checked arithmetic and refuses them against the caller's
//! [`BlurLimits`] before it allocates.
#![deny(clippy::arithmetic_side_effects)]
use crate::render_plan::SampleRange;
use crate::Rect;
use crate::blend_math::Exactness;
use crate::coverage_math::{CoverageError, feather_kernel, feather_support};
use serde::Serialize;
use std::fmt;

/// Class of both blur operators (see the module docs).
pub const BLUR_EXACTNESS: Exactness = Exactness::Approximate;
/// Largest blur radius in pixels (Folio `MAX_BLUR_RADIUS`, D19 (f) engineering bound).
pub const MAX_BLUR_RADIUS_PX: f32 = 1_000.0;
/// Bytes of one window sample (`[f32; 4]`).
pub const BLUR_SAMPLE_BYTES: u64 = 16;

/// Which pixels a blur acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum BlurScope {
    /// The layer's own content (Figma layer blur, Photoshop Gaussian Blur on the layer).
    Layer,
    /// The backdrop below the layer, inside the layer's shape (Figma background blur).
    Backdrop,
}
impl BlurScope {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Layer => "blur_layer",
            Self::Backdrop => "blur_backdrop",
        }
    }
}

/// A blur of one layer ([`crate::render_plan::LayerExtension::blur`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct LayerBlur {
    pub scope: BlurScope,
    /// Radius in plan pixels, `0..=MAX_BLUR_RADIUS_PX` (0 = no blur, the identity kernel).
    pub radius_px: f32,
}

/// Typed failure of the blur maths.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlurError {
    /// Radius non-finite, negative or above [`MAX_BLUR_RADIUS_PX`].
    Radius,
    /// A buffer length disagrees with its rectangle, or the kernel is malformed.
    Shape,
    /// The bytes or work of a window exceed the caller's limits (refused before allocation).
    OverBudget { needed: u64, limit: u64 },
    /// A sample outside `[0, 1]` or non-finite.
    OutOfDomain,
    Overflow,
    /// A kernel parameter other than the Gaussian radius (box half-width/iterations, angle, length,
    /// centre, amount) is non-finite or outside its declared range (lane SMB-PIGMENT-AE-KERNELS-1).
    Parameter,
}
impl BlurError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Radius => "blur_radius",
            Self::Shape => "blur_shape",
            Self::OverBudget { .. } => "blur_over_budget",
            Self::OutOfDomain => "blur_out_of_domain",
            Self::Overflow => "blur_overflow",
            Self::Parameter => "blur_parameter",
        }
    }
}
impl fmt::Display for BlurError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for BlurError {}

fn radius_error(_: CoverageError) -> BlurError {
    BlurError::Radius
}

impl LayerBlur {
    pub const fn new(scope: BlurScope, radius_px: f32) -> Self {
        Self { scope, radius_px }
    }
    /// The radius is finite and inside `0..=MAX_BLUR_RADIUS_PX`.
    pub fn validate(&self) -> Result<(), BlurError> {
        if self.radius_px.is_finite() && (0.0..=MAX_BLUR_RADIUS_PX).contains(&self.radius_px) {
            Ok(())
        } else {
            Err(BlurError::Radius)
        }
    }
    /// Half-width of the kernel in pixels: the margin a backend must read around a pixel.
    pub fn support_px(&self) -> Result<u32, BlurError> {
        self.validate()?;
        feather_support(self.radius_px).map_err(radius_error)
    }
    /// The normalised taps `[-support ..= support]` (`feather_kernel`, the shared Gaussian).
    pub fn taps(&self) -> Result<Vec<f32>, BlurError> {
        self.validate()?;
        feather_kernel(self.radius_px).map_err(radius_error)
    }
}

/// Limits of one [`blur_window`] call: owned bytes at the peak and work (tap visits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlurLimits {
    pub max_bytes: u64,
    pub max_work: u64,
}

/// Predicted peak bytes and work of [`blur_window`] for an output window of `out` with `taps`
/// taps (the horizontal pass covers `out.width x (out.height + 2 support)`).
pub fn blur_window_cost(out: Rect, taps: usize) -> Result<(u64, u64), BlurError> {
    let taps = u64::try_from(taps).map_err(|_| BlurError::Overflow)?;
    if taps == 0 || taps.is_multiple_of(2) {
        return Err(BlurError::Shape);
    }
    let support = taps.checked_sub(1).ok_or(BlurError::Shape)?.checked_div(2).ok_or(BlurError::Shape)?;
    let rows = u64::from(out.height)
        .checked_add(support.checked_mul(2).ok_or(BlurError::Overflow)?)
        .ok_or(BlurError::Overflow)?;
    let tmp = u64::from(out.width).checked_mul(rows).ok_or(BlurError::Overflow)?;
    let area = u64::from(out.width)
        .checked_mul(u64::from(out.height))
        .ok_or(BlurError::Overflow)?;
    let samples = tmp.checked_add(area).ok_or(BlurError::Overflow)?;
    let bytes = samples.checked_mul(BLUR_SAMPLE_BYTES).ok_or(BlurError::Overflow)?;
    let work = samples.checked_mul(taps).ok_or(BlurError::Overflow)?;
    Ok((bytes, work))
}

fn count(r: Rect) -> Result<usize, BlurError> {
    usize::try_from(u64::from(r.width).checked_mul(u64::from(r.height)).ok_or(BlurError::Overflow)?)
        .map_err(|_| BlurError::Overflow)
}

/// Premultiplied straight sample, refused outside `[0, 1]` (unit range) or, in the extended range
/// (lane SMB-PIGMENT-EXTENDED-RANGE-2), for a non-finite colour or an alpha outside `[0, 1]`.
fn premultiplied(p: [f32; 4], range: SampleRange) -> Result<[f64; 4], BlurError> {
    let colour_ok = |v: &f32| v.is_finite() && (range == SampleRange::Extended || (0.0..=1.0).contains(v));
    if !(p[..3].iter().all(colour_ok) && p[3].is_finite() && (0.0..=1.0).contains(&p[3])) {
        return Err(BlurError::OutOfDomain);
    }
    let a = f64::from(p[3]);
    Ok([f64::from(p[0]) * a, f64::from(p[1]) * a, f64::from(p[2]) * a, a])
}

/// Straight RGBA from a premultiplied binary32 sample: `A <= 0` is transparent, alpha at most 1,
/// colour `clamp(P / A, 0, 1)` (the rule of `transform_math`'s premultiplied resampling).
pub fn unpremultiply(p: [f32; 4]) -> [f32; 4] {
    unpremultiply_in(SampleRange::Unit, p)
}

/// [`unpremultiply`] in `range`: the extended range keeps the colour `P / A` unclamped (a gaussian
/// blur is a convex combination; nothing in it refers to the unit white point, STU-COL-120).
pub fn unpremultiply_in(range: SampleRange, p: [f32; 4]) -> [f32; 4] {
    let a = p[3];
    if a.is_nan() || a <= 0.0 {
        return [0.0; 4];
    }
    let c = |v: f32| {
        let q = v / a;
        if q.is_nan() {
            0.0
        } else if range == SampleRange::Extended {
            q
        } else {
            q.clamp(0.0, 1.0)
        }
    };
    [c(p[0]), c(p[1]), c(p[2]), a.min(1.0)]
}

/// The blur of the straight RGBA plane `src` (row-major over `src_rect`, transparent beyond it)
/// at every pixel of `out`, straight RGBA row-major over `out`. Separable: the horizontal pass over
/// the `out` columns and the `out` rows grown by the support, then the vertical pass; each pass
/// sums `tap * premultiplied sample` in binary64 in tap order and rounds once to binary32; the
/// result is [`unpremultiply`]d. The bytes and work ([`blur_window_cost`]) are checked against
/// `limits` before any allocation.
pub fn blur_window(
    src: &[[f32; 4]],
    src_rect: Rect,
    out: Rect,
    taps: &[f32],
    limits: BlurLimits,
) -> Result<Vec<[f32; 4]>, BlurError> {
    blur_window_in(SampleRange::Unit, src, src_rect, out, taps, limits)
}

/// [`blur_window`] in `range`: the extended range admits any finite colour and keeps the blurred
/// colour unclamped; a non-finite result is [`BlurError::OutOfDomain`].
pub fn blur_window_in(
    range: SampleRange,
    src: &[[f32; 4]],
    src_rect: Rect,
    out: Rect,
    taps: &[f32],
    limits: BlurLimits,
) -> Result<Vec<[f32; 4]>, BlurError> {
    if src.len() != count(src_rect)? {
        return Err(BlurError::Shape);
    }
    let (bytes, work) = blur_window_cost(out, taps.len())?;
    if bytes > limits.max_bytes {
        return Err(BlurError::OverBudget { needed: bytes, limit: limits.max_bytes });
    }
    if work > limits.max_work {
        return Err(BlurError::OverBudget { needed: work, limit: limits.max_work });
    }
    let support = i64::try_from(taps.len().checked_div(2).ok_or(BlurError::Shape)?)
        .map_err(|_| BlurError::Overflow)?;
    let (sx1, sy1) = src_rect.end().map_err(|_| BlurError::Overflow)?;
    out.end().map_err(|_| BlurError::Overflow)?;
    let src_width = usize::try_from(src_rect.width).map_err(|_| BlurError::Overflow)?;
    let at_src = |x: i64, y: i64| -> Result<Option<[f32; 4]>, BlurError> {
        if x < src_rect.x || y < src_rect.y || x >= sx1 || y >= sy1 {
            return Ok(None);
        }
        let row = usize::try_from(y.checked_sub(src_rect.y).ok_or(BlurError::Overflow)?)
            .map_err(|_| BlurError::Overflow)?;
        let column = usize::try_from(x.checked_sub(src_rect.x).ok_or(BlurError::Overflow)?)
            .map_err(|_| BlurError::Overflow)?;
        let at = row
            .checked_mul(src_width)
            .and_then(|r| r.checked_add(column))
            .ok_or(BlurError::Overflow)?;
        src.get(at).copied().map(Some).ok_or(BlurError::Shape)
    };
    let width = usize::try_from(out.width).map_err(|_| BlurError::Overflow)?;
    let height = usize::try_from(out.height).map_err(|_| BlurError::Overflow)?;
    // Radius 0 (the identity kernel `[1.0]`) is no blur: the samples are copied unchanged (no
    // premultiplied round trip), transparent beyond `src_rect`.
    if taps == [1.0] {
        let mut copy: Vec<[f32; 4]> = Vec::new();
        copy.try_reserve_exact(width.checked_mul(height).ok_or(BlurError::Overflow)?)
            .map_err(|_| BlurError::OverBudget { needed: bytes, limit: limits.max_bytes })?;
        let (ox1, oy1) = out.end().map_err(|_| BlurError::Overflow)?;
        for y in out.y..oy1 {
            for x in out.x..ox1 {
                let p = at_src(x, y)?.unwrap_or([0.0; 4]);
                premultiplied(p, range)?;
                copy.push(p);
            }
        }
        return Ok(copy);
    }
    let twice = usize::try_from(support.checked_mul(2).ok_or(BlurError::Overflow)?)
        .map_err(|_| BlurError::Overflow)?;
    let rows = height.checked_add(twice).ok_or(BlurError::Overflow)?;
    // Pass 1 (horizontal, premultiplied): buffer row `r` is plan row `out.y + r - support`.
    let mut tmp: Vec<[f32; 4]> = Vec::new();
    tmp.try_reserve_exact(width.checked_mul(rows).ok_or(BlurError::Overflow)?)
        .map_err(|_| BlurError::OverBudget { needed: bytes, limit: limits.max_bytes })?;
    for r in 0..rows {
        let y = out
            .y
            .checked_add(i64::try_from(r).map_err(|_| BlurError::Overflow)?)
            .and_then(|y| y.checked_sub(support))
            .ok_or(BlurError::Overflow)?;
        for c in 0..width {
            let x0 = out
                .x
                .checked_add(i64::try_from(c).map_err(|_| BlurError::Overflow)?)
                .and_then(|x| x.checked_sub(support))
                .ok_or(BlurError::Overflow)?;
            let mut sum = [0.0_f64; 4];
            if y >= src_rect.y && y < sy1 {
                for (t, w) in taps.iter().enumerate() {
                    let x = x0
                        .checked_add(i64::try_from(t).map_err(|_| BlurError::Overflow)?)
                        .ok_or(BlurError::Overflow)?;
                    if let Some(p) = at_src(x, y)? {
                        let p = premultiplied(p, range)?;
                        let w = f64::from(*w);
                        for (s, v) in sum.iter_mut().zip(p) {
                            *s += w * v;
                        }
                    }
                }
            }
            tmp.push(sum.map(|v| v as f32));
        }
    }
    // Pass 2 (vertical): output row `rr` reads buffer rows `rr ..= rr + 2 support`.
    let mut result: Vec<[f32; 4]> = Vec::new();
    result
        .try_reserve_exact(width.checked_mul(height).ok_or(BlurError::Overflow)?)
        .map_err(|_| BlurError::OverBudget { needed: bytes, limit: limits.max_bytes })?;
    for rr in 0..height {
        for c in 0..width {
            let mut sum = [0.0_f64; 4];
            for (t, w) in taps.iter().enumerate() {
                let at = rr
                    .checked_add(t)
                    .and_then(|r| r.checked_mul(width))
                    .and_then(|r| r.checked_add(c))
                    .ok_or(BlurError::Overflow)?;
                let p = tmp.get(at).ok_or(BlurError::Shape)?;
                let w = f64::from(*w);
                for (s, v) in sum.iter_mut().zip(p) {
                    *s += w * f64::from(*v);
                }
            }
            let px = unpremultiply_in(range, sum.map(|v| v as f32));
            if !px.iter().all(|v| v.is_finite()) {
                return Err(BlurError::OutOfDomain);
            }
            result.push(px);
        }
    }
    Ok(result)
}

/// Backdrop blur inside the layer shape: the premultiplied lerp from `backdrop` to `blurred` (both
/// straight RGBA) by `k` in `[0, 1]`, returned straight ([`unpremultiply`]). binary32 arithmetic in
/// this order (render-gpu transcribes it): `P = Pb + (Pbl - Pb) * k` per premultiplied channel.
pub fn backdrop_mix(backdrop: [f32; 4], blurred: [f32; 4], k: f32) -> Result<[f32; 4], BlurError> {
    backdrop_mix_in(SampleRange::Unit, backdrop, blurred, k)
}

/// [`backdrop_mix`] in `range` (extended: finite colours, unclamped result; a non-finite result is
/// `OutOfDomain`).
pub fn backdrop_mix_in(
    range: SampleRange,
    backdrop: [f32; 4],
    blurred: [f32; 4],
    k: f32,
) -> Result<[f32; 4], BlurError> {
    if !(k.is_finite() && (0.0..=1.0).contains(&k)) {
        return Err(BlurError::OutOfDomain);
    }
    premultiplied(backdrop, range)?;
    premultiplied(blurred, range)?;
    let pre = |p: [f32; 4]| [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]];
    let (b, l) = (pre(backdrop), pre(blurred));
    let mut out = [0.0_f32; 4];
    for ((o, bv), lv) in out.iter_mut().zip(b).zip(l) {
        *o = bv + (lv - bv) * k;
    }
    let px = unpremultiply_in(range, out);
    if !px.iter().all(|v| v.is_finite()) {
        return Err(BlurError::OutOfDomain);
    }
    Ok(px)
}

// ---------------------------------------------------------------------------------------------
// After Effects blur kernels (lane SMB-PIGMENT-AE-KERNELS-1, for composite's effect families:
// Fast Box Blur, Blur/Glow Dimensions, Directional Blur, Radial Blur). Studio readings; the After
// Effects / Photoshop formulas, units and sample counts are UNVERIFIED (D5), class Approximate.
//
// Shared conventions of every kernel below:
// - straight RGBA in and out, row-major, transparent beyond `src_rect`; the work is done
//   premultiplied (refused outside the domain of `range` like [`blur_window_in`]) and the result is
//   [`unpremultiply_in`]d; a non-finite result is `OutOfDomain`;
// - every output pixel depends only on the source and the parameters, never on the window, so any
//   split of a plane into windows gives the same bytes;
// - sums run in binary64 in a fixed order (tap / sample index ascending, then the bilinear corners
//   in the order (x0, y0), (x0 + 1, y0), (x0, y0 + 1), (x0 + 1, y0 + 1)) and round once to binary32;
// - the declared sub-pixel filter ([`SUBPIXEL_FILTER`]) is bilinear on premultiplied samples, a
//   corner whose weight is exactly 0 is not read; pixel `(x, y)` covers `[x, x + 1) x [y, y + 1)`
//   in plan coordinates and is sampled at its centre `(x + 0.5, y + 0.5)`;
// - bytes and work are predicted with checked arithmetic and refused against [`BlurLimits`] before
//   any allocation; parameters are range-checked (never clamped), `BlurError::Parameter`.
// ---------------------------------------------------------------------------------------------

/// Most taps of a [`box_kernel`] (the Fast Box Blur bound composite used for its own tap vector).
pub const MAX_BOX_KERNEL_TAPS: usize = 6_001;
/// Largest [`directional_blur_window`] length in pixels (`2 ceil(length / 2) + 1` samples stay
/// within [`MAX_BOX_KERNEL_TAPS`]).
pub const MAX_DIRECTIONAL_BLUR_LENGTH_PX: f32 = 6_000.0;
/// Largest [`radial_blur_window`] amount (both kinds; see [`RadialBlurKind`]).
pub const MAX_RADIAL_BLUR_AMOUNT: f32 = 100.0;
/// Most samples on one side of a radial blur path (`2 m + 1` samples per pixel, `m` at most this).
pub const MAX_RADIAL_BLUR_HALF_SAMPLES: u32 = 256;
/// The declared sub-pixel filter of the directional and radial blurs.
pub const SUBPIXEL_FILTER: &str = "bilinear_premultiplied";
/// Bilinear corners read per path sample (the work unit of the directional and radial blurs).
const BILINEAR_CORNERS: u64 = 4;

/// Axis of a one-axis separable pass ([`blur_window_axis`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum BlurAxis {
    /// Along x (each output pixel reads its row).
    Horizontal,
    /// Along y (each output pixel reads its column).
    Vertical,
}

/// Path of a radial blur ([`radial_blur_window`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum RadialBlurKind {
    /// Samples on the circle through the pixel around the centre, total arc `amount` degrees
    /// centred on the pixel (`-amount / 2 ..= amount / 2`).
    Spin,
    /// Samples on the ray from the centre through the pixel, at the scales
    /// `1 - amount / 200 ..= 1 + amount / 200` of the pixel's offset from the centre.
    Zoom,
}

/// The normalised taps `[-support ..= support]` of `iterations` passes of a box `2 half_width_px +
/// 1` wide (support `half_width_px * iterations`): the box convolved with itself, so 3 iterations
/// approximate a Gaussian of variance `iterations * half_width (half_width + 1) / 3` (test
/// `tests/ae_kernels.rs`). `half_width_px` or `iterations` 0 is the identity `[1.0]`. Each pass is
/// the running sum over the previous taps divided by the box width (binary64, index order, negative
/// rounding residue clamped to 0), then the taps are divided by their sum (index order) and rounded
/// once to binary32: the same arithmetic as the tap vector composite built itself (`box_taps`).
/// More than [`MAX_BOX_KERNEL_TAPS`] taps is [`BlurError::Parameter`].
pub fn box_kernel(half_width_px: u32, iterations: u32) -> Result<Vec<f32>, BlurError> {
    if half_width_px == 0 || iterations == 0 {
        return Ok(vec![1.0]);
    }
    let half = usize::try_from(half_width_px).map_err(|_| BlurError::Parameter)?;
    let iters = usize::try_from(iterations).map_err(|_| BlurError::Parameter)?;
    let width = half.checked_mul(2).and_then(|v| v.checked_add(1)).ok_or(BlurError::Parameter)?;
    let len = half
        .checked_mul(2)
        .and_then(|v| v.checked_mul(iters))
        .and_then(|v| v.checked_add(1))
        .ok_or(BlurError::Parameter)?;
    if len > MAX_BOX_KERNEL_TAPS {
        return Err(BlurError::Parameter);
    }
    let fw = f64::from(half_width_px) * 2.0 + 1.0;
    let grow = width.checked_sub(1).ok_or(BlurError::Overflow)?;
    let mut cur = vec![1.0_f64];
    for _ in 0..iterations {
        let n = cur.len().checked_add(grow).ok_or(BlurError::Overflow)?;
        let mut next = Vec::with_capacity(n);
        let mut acc = 0.0_f64;
        for i in 0..n {
            if let Some(v) = cur.get(i) {
                acc += v;
            }
            if let Some(v) = i.checked_sub(width).and_then(|j| cur.get(j)) {
                acc -= v;
            }
            next.push((acc / fw).max(0.0));
        }
        cur = next;
    }
    let mut sum = 0.0_f64;
    for v in &cur {
        sum += v;
    }
    Ok(cur.iter().map(|v| (v / sum) as f32).collect())
}

/// Predicted peak bytes (the output) and work (tap visits) of [`blur_window_axis`].
pub fn blur_window_axis_cost(out: Rect, taps: usize) -> Result<(u64, u64), BlurError> {
    let t = u64::try_from(taps).map_err(|_| BlurError::Overflow)?;
    if t == 0 || t.is_multiple_of(2) {
        return Err(BlurError::Shape);
    }
    let area = u64::from(out.width).checked_mul(u64::from(out.height)).ok_or(BlurError::Overflow)?;
    let bytes = area.checked_mul(BLUR_SAMPLE_BYTES).ok_or(BlurError::Overflow)?;
    let work = area.checked_mul(t).ok_or(BlurError::Overflow)?;
    Ok((bytes, work))
}

fn check_budget(bytes: u64, work: u64, limits: BlurLimits) -> Result<(), BlurError> {
    if bytes > limits.max_bytes {
        return Err(BlurError::OverBudget { needed: bytes, limit: limits.max_bytes });
    }
    if work > limits.max_work {
        return Err(BlurError::OverBudget { needed: work, limit: limits.max_work });
    }
    Ok(())
}

/// The source sample at plan pixel `(x, y)`, `None` beyond `src_rect`.
fn fetch(src: &[[f32; 4]], src_rect: Rect, x: i64, y: i64) -> Result<Option<[f32; 4]>, BlurError> {
    let (Some(dx), Some(dy)) = (x.checked_sub(src_rect.x), y.checked_sub(src_rect.y)) else {
        return Ok(None);
    };
    let (Ok(dx), Ok(dy)) = (u32::try_from(dx), u32::try_from(dy)) else {
        return Ok(None);
    };
    if dx >= src_rect.width || dy >= src_rect.height {
        return Ok(None);
    }
    let width = usize::try_from(src_rect.width).map_err(|_| BlurError::Overflow)?;
    let at = usize::try_from(dy)
        .ok()
        .and_then(|r| r.checked_mul(width))
        .and_then(|r| r.checked_add(usize::try_from(dx).ok()?))
        .ok_or(BlurError::Overflow)?;
    src.get(at).copied().map(Some).ok_or(BlurError::Shape)
}

/// Common entry checks: the source shape, both rectangle ends, the budget, then the output buffer.
fn begin(
    src: &[[f32; 4]],
    src_rect: Rect,
    out: Rect,
    bytes: u64,
    work: u64,
    limits: BlurLimits,
) -> Result<Vec<[f32; 4]>, BlurError> {
    if src.len() != count(src_rect)? {
        return Err(BlurError::Shape);
    }
    src_rect.end().map_err(|_| BlurError::Overflow)?;
    out.end().map_err(|_| BlurError::Overflow)?;
    check_budget(bytes, work, limits)?;
    let mut v: Vec<[f32; 4]> = Vec::new();
    v.try_reserve_exact(count(out)?)
        .map_err(|_| BlurError::OverBudget { needed: bytes, limit: limits.max_bytes })?;
    Ok(v)
}

/// The identity of every kernel below: the samples copied unchanged (no premultiplied round trip),
/// domain-checked, transparent beyond `src_rect`.
fn identity_into(
    range: SampleRange,
    src: &[[f32; 4]],
    src_rect: Rect,
    out: Rect,
    mut v: Vec<[f32; 4]>,
) -> Result<Vec<[f32; 4]>, BlurError> {
    let (ox1, oy1) = out.end().map_err(|_| BlurError::Overflow)?;
    for y in out.y..oy1 {
        for x in out.x..ox1 {
            let p = fetch(src, src_rect, x, y)?.unwrap_or([0.0; 4]);
            premultiplied(p, range)?;
            v.push(p);
        }
    }
    Ok(v)
}

fn finish(range: SampleRange, sum: [f64; 4]) -> Result<[f32; 4], BlurError> {
    let px = unpremultiply_in(range, sum.map(|v| v as f32));
    if px.iter().all(|v| v.is_finite()) { Ok(px) } else { Err(BlurError::OutOfDomain) }
}

/// One separable pass of `taps` along `axis` (the one-axis form of [`blur_window_in`], for After
/// Effects' Blur Dimensions Horizontal / Vertical): output pixel `(x, y)` is
/// `sum_t taps[t] * premultiplied(src at (x + t - support, y))` (horizontal; vertical offsets y),
/// binary64 in tap order, rounded once to binary32 and [`unpremultiply_in`]d. A horizontal pass over
/// the window grown by the support in y followed by a vertical pass equals [`blur_window_in`] up to
/// the straight round trip between the passes (within 1e-6 on unit samples, test
/// `tests/ae_kernels.rs`). Taps `[1.0]` are the identity. Cost: [`blur_window_axis_cost`].
pub fn blur_window_axis(
    range: SampleRange,
    src: &[[f32; 4]],
    src_rect: Rect,
    out: Rect,
    taps: &[f32],
    axis: BlurAxis,
    limits: BlurLimits,
) -> Result<Vec<[f32; 4]>, BlurError> {
    let (bytes, work) = blur_window_axis_cost(out, taps.len())?;
    if !taps.iter().all(|t| t.is_finite()) {
        return Err(BlurError::Shape);
    }
    let mut result = begin(src, src_rect, out, bytes, work, limits)?;
    if taps == [1.0] {
        return identity_into(range, src, src_rect, out, result);
    }
    let support = i64::try_from(taps.len().checked_div(2).ok_or(BlurError::Shape)?)
        .map_err(|_| BlurError::Overflow)?;
    let (ox1, oy1) = out.end().map_err(|_| BlurError::Overflow)?;
    for y in out.y..oy1 {
        for x in out.x..ox1 {
            let mut sum = [0.0_f64; 4];
            for (t, w) in taps.iter().enumerate() {
                let d = i64::try_from(t)
                    .ok()
                    .and_then(|t| t.checked_sub(support))
                    .ok_or(BlurError::Overflow)?;
                let at = match axis {
                    BlurAxis::Horizontal => x.checked_add(d).map(|sx| (sx, y)),
                    BlurAxis::Vertical => y.checked_add(d).map(|sy| (x, sy)),
                };
                let Some((sx, sy)) = at else { continue };
                if let Some(p) = fetch(src, src_rect, sx, sy)? {
                    let p = premultiplied(p, range)?;
                    let w = f64::from(*w);
                    for (s, v) in sum.iter_mut().zip(p) {
                        *s += w * v;
                    }
                }
            }
            result.push(finish(range, sum)?);
        }
    }
    Ok(result)
}

/// `(cos, sin)` of `angle_deg`, exact at multiples of 90 degrees (the angle is reduced into
/// `[0, 360)` first; other angles use the platform's binary64 `sin_cos`).
fn cos_sin_deg(angle_deg: f64) -> (f64, f64) {
    let a = angle_deg.rem_euclid(360.0);
    if a == 0.0 {
        (1.0, 0.0)
    } else if a == 90.0 {
        (0.0, 1.0)
    } else if a == 180.0 {
        (-1.0, 0.0)
    } else if a == 270.0 {
        (0.0, -1.0)
    } else {
        let (s, c) = a.to_radians().sin_cos();
        (c, s)
    }
}

/// Bilinear premultiplied sample at pixel-index position `(u, v)` (pixel `(x, y)` sits at `(x, y)`;
/// [`SUBPIXEL_FILTER`]) added into `acc` (corner order of the section docs; zero weights not read).
fn bilinear_add(
    range: SampleRange,
    src: &[[f32; 4]],
    src_rect: Rect,
    u: f64,
    v: f64,
    acc: &mut [f64; 4],
) -> Result<(), BlurError> {
    let (fx, fy) = (u.floor(), v.floor());
    if !(fx.is_finite() && fy.is_finite()) {
        return Err(BlurError::Overflow);
    }
    let (tx, ty) = (u - fx, v - fy);
    let (x0, y0) = (fx as i64, fy as i64);
    let corners = [
        ((1.0 - tx) * (1.0 - ty), Some(x0), Some(y0)),
        (tx * (1.0 - ty), x0.checked_add(1), Some(y0)),
        ((1.0 - tx) * ty, Some(x0), y0.checked_add(1)),
        (tx * ty, x0.checked_add(1), y0.checked_add(1)),
    ];
    for (w, x, y) in corners {
        if w == 0.0 {
            continue;
        }
        let (Some(x), Some(y)) = (x, y) else { continue };
        if let Some(p) = fetch(src, src_rect, x, y)? {
            let p = premultiplied(p, range)?;
            for (s, c) in acc.iter_mut().zip(p) {
                *s += w * c;
            }
        }
    }
    Ok(())
}

fn path_cost(out: Rect, samples_total: u64) -> Result<(u64, u64), BlurError> {
    let area = u64::from(out.width).checked_mul(u64::from(out.height)).ok_or(BlurError::Overflow)?;
    let bytes = area.checked_mul(BLUR_SAMPLE_BYTES).ok_or(BlurError::Overflow)?;
    let work = samples_total.checked_mul(BILINEAR_CORNERS).ok_or(BlurError::Overflow)?;
    Ok((bytes, work))
}

/// Half sample count `m = ceil(length / 2)` of a directional blur (`2 m + 1` samples), checked.
fn directional_half(length_px: f32) -> Result<u32, BlurError> {
    if !(length_px.is_finite() && (0.0..=MAX_DIRECTIONAL_BLUR_LENGTH_PX).contains(&length_px)) {
        return Err(BlurError::Parameter);
    }
    Ok((f64::from(length_px) / 2.0).ceil() as u32)
}

/// Predicted peak bytes (the output) and work (bilinear corner reads) of
/// [`directional_blur_window`].
pub fn directional_blur_window_cost(out: Rect, length_px: f32) -> Result<(u64, u64), BlurError> {
    let m = u64::from(directional_half(length_px)?);
    let n = m.checked_mul(2).and_then(|v| v.checked_add(1)).ok_or(BlurError::Overflow)?;
    let area = u64::from(out.width).checked_mul(u64::from(out.height)).ok_or(BlurError::Overflow)?;
    path_cost(out, area.checked_mul(n).ok_or(BlurError::Overflow)?)
}

/// Directional (motion) blur: the mean of `2 m + 1` samples, `m = ceil(length_px / 2)`, evenly
/// spaced on the segment of length `length_px` centred on the pixel along `angle_deg` (Studio
/// reading of After Effects' Directional Blur, UNVERIFIED D5: uniform weights, the segment's ends
/// are its first and last sample centres). `angle_deg` is measured from +x towards screen-up (y
/// down, so the direction is `(cos, -sin)`; any finite angle, reduced modulo 360; the kernel is
/// symmetric, so `a` and `a + 180` agree); sample `k` in `-m ..= m` sits at offset
/// `k * (length / 2m)` along it and is read with [`SUBPIXEL_FILTER`]. Angle 0 with an even integer
/// length `2h` is the horizontal box of [`box_kernel`]`(h, 1)`; length 0 is the identity.
/// `length_px` is `0..=MAX_DIRECTIONAL_BLUR_LENGTH_PX`. Cost: [`directional_blur_window_cost`].
pub fn directional_blur_window(
    range: SampleRange,
    src: &[[f32; 4]],
    src_rect: Rect,
    out: Rect,
    angle_deg: f32,
    length_px: f32,
    limits: BlurLimits,
) -> Result<Vec<[f32; 4]>, BlurError> {
    if !angle_deg.is_finite() {
        return Err(BlurError::Parameter);
    }
    let (bytes, work) = directional_blur_window_cost(out, length_px)?;
    let m = directional_half(length_px)?;
    let mut result = begin(src, src_rect, out, bytes, work, limits)?;
    if m == 0 {
        return identity_into(range, src, src_rect, out, result);
    }
    let (c, s) = cos_sin_deg(f64::from(angle_deg));
    let (dx, dy) = (c, -s);
    let step = f64::from(length_px) / 2.0 / f64::from(m);
    let n = f64::from(m) * 2.0 + 1.0;
    let mi = i64::from(m);
    let (ox1, oy1) = out.end().map_err(|_| BlurError::Overflow)?;
    for y in out.y..oy1 {
        for x in out.x..ox1 {
            let mut sum = [0.0_f64; 4];
            for k in mi.checked_neg().ok_or(BlurError::Overflow)?..=mi {
                let o = k as f64 * step;
                bilinear_add(range, src, src_rect, x as f64 + o * dx, y as f64 + o * dy, &mut sum)?;
            }
            result.push(finish(range, sum.map(|v| v / n))?);
        }
    }
    Ok(result)
}

fn radial_params(centre: [f32; 2], amount: f32) -> Result<(f64, f64), BlurError> {
    let finite = centre.iter().all(|v| v.is_finite()) && amount.is_finite();
    if !(finite && (0.0..=MAX_RADIAL_BLUR_AMOUNT).contains(&amount)) {
        return Err(BlurError::Parameter);
    }
    // Pixel-index space: pixel (x, y) sits at (x, y), so the plan centre moves by -0.5.
    Ok((f64::from(centre[0]) - 0.5, f64::from(centre[1]) - 0.5))
}

/// The half sample count `m = min(ceil(extent / 2), MAX_RADIAL_BLUR_HALF_SAMPLES)` at offset
/// `(ox, oy)` from the centre (sample spacing at most 1 px below the cap); the path extent is the
/// arc length `r * amount` (radians, spin) or the radial span `r * amount / 100` (zoom).
fn radial_half(kind: RadialBlurKind, amount: f32, ox: f64, oy: f64) -> u32 {
    let r = (ox * ox + oy * oy).sqrt();
    let extent = match kind {
        RadialBlurKind::Spin => r * f64::from(amount).to_radians(),
        RadialBlurKind::Zoom => r * f64::from(amount) / 100.0,
    };
    let half = (extent / 2.0).ceil();
    if half.is_nan() || half <= 0.0 {
        0
    } else if half >= f64::from(MAX_RADIAL_BLUR_HALF_SAMPLES) {
        MAX_RADIAL_BLUR_HALF_SAMPLES
    } else {
        half as u32
    }
}

/// Predicted peak bytes (the output) and work (bilinear corner reads, the exact per-pixel sample
/// counts summed over `out`) of [`radial_blur_window`].
pub fn radial_blur_window_cost(
    out: Rect,
    centre: [f32; 2],
    amount: f32,
    kind: RadialBlurKind,
) -> Result<(u64, u64), BlurError> {
    let (cx, cy) = radial_params(centre, amount)?;
    let (ox1, oy1) = out.end().map_err(|_| BlurError::Overflow)?;
    let mut samples = 0u64;
    for y in out.y..oy1 {
        for x in out.x..ox1 {
            let m = u64::from(radial_half(kind, amount, x as f64 - cx, y as f64 - cy));
            let n = m.checked_mul(2).and_then(|v| v.checked_add(1)).ok_or(BlurError::Overflow)?;
            samples = samples.checked_add(n).ok_or(BlurError::Overflow)?;
        }
    }
    path_cost(out, samples)
}

/// Radial blur around `centre` (plan coordinates, pixel `(x, y)` centred at `(x + 0.5, y + 0.5)`):
/// the mean of `2 m + 1` samples on the pixel's path ([`RadialBlurKind`]), `m` per pixel from its
/// path extent (spacing at most 1 px, at most [`MAX_RADIAL_BLUR_HALF_SAMPLES`]), read with
/// [`SUBPIXEL_FILTER`]. With `d` the pixel's offset from the centre, sample `k` in `-m ..= m` sits
/// at `pixel + rot(k * amount / 2m degrees) d - d` (spin) or `pixel + (k * amount / 200m) d`
/// (zoom), so sample 0 is the pixel itself. Studio reading of After Effects' / Photoshop's Radial
/// Blur (amount unit, sample counts and quality modes UNVERIFIED, D5): `amount` is
/// `0..=MAX_RADIAL_BLUR_AMOUNT` for both kinds; amount 0 is the identity. Spin moves colour only
/// along circles about the centre (distances kept), zoom only along rays (angles kept); test
/// `tests/ae_kernels.rs`. Cost: [`radial_blur_window_cost`].
#[allow(clippy::too_many_arguments)]
pub fn radial_blur_window(
    range: SampleRange,
    src: &[[f32; 4]],
    src_rect: Rect,
    out: Rect,
    centre: [f32; 2],
    amount: f32,
    kind: RadialBlurKind,
    limits: BlurLimits,
) -> Result<Vec<[f32; 4]>, BlurError> {
    let (bytes, work) = radial_blur_window_cost(out, centre, amount, kind)?;
    let (cx, cy) = radial_params(centre, amount)?;
    let mut result = begin(src, src_rect, out, bytes, work, limits)?;
    if amount == 0.0 {
        return identity_into(range, src, src_rect, out, result);
    }
    let (ox1, oy1) = out.end().map_err(|_| BlurError::Overflow)?;
    for y in out.y..oy1 {
        for x in out.x..ox1 {
            let (px, py) = (x as f64, y as f64);
            let (ox, oy) = (px - cx, py - cy);
            let m = radial_half(kind, amount, ox, oy);
            let mi = i64::from(m);
            let unit = match kind {
                RadialBlurKind::Spin => f64::from(amount).to_radians() / 2.0 / f64::from(m.max(1)),
                RadialBlurKind::Zoom => f64::from(amount) / 200.0 / f64::from(m.max(1)),
            };
            let mut sum = [0.0_f64; 4];
            for k in mi.checked_neg().ok_or(BlurError::Overflow)?..=mi {
                let t = k as f64 * unit;
                let (u, v) = match kind {
                    RadialBlurKind::Spin => {
                        let (s, c) = if k == 0 { (0.0, 1.0) } else { t.sin_cos() };
                        (px + ((ox * c - oy * s) - ox), py + ((ox * s + oy * c) - oy))
                    }
                    RadialBlurKind::Zoom => (px + ox * t, py + oy * t),
                };
                bilinear_add(range, src, src_rect, u, v, &mut sum)?;
            }
            let n = f64::from(m) * 2.0 + 1.0;
            result.push(finish(range, sum.map(|v| v / n))?);
        }
    }
    Ok(result)
}
