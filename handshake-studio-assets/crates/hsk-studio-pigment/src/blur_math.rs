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
}
impl BlurError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Radius => "blur_radius",
            Self::Shape => "blur_shape",
            Self::OverBudget { .. } => "blur_over_budget",
            Self::OutOfDomain => "blur_out_of_domain",
            Self::Overflow => "blur_overflow",
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
