//! Windowed effect mattes (single owner, CX-CAP-001, of the tile-sized evaluation of the
//! [`super`] reference maths): the matte and the composed contribution of an effect stack for an
//! OUTPUT WINDOW of an arbitrarily large layer.
//!
//! Contract (edge policy and identity):
//! - Layer alpha is read through a caller-supplied [`AlphaSource`] and only for the window grown by
//!   [`super::effect_support_px`] (checked by `tests/effect_window.rs`
//!   `source_reads_stay_inside_the_support_margin`, which records every requested rectangle and
//!   checks it against the margin and the layer). Alpha beyond [`WindowGeometry::layer`] is 0 and
//!   the accessor is not asked for it.
//! - [`WindowGeometry::domain`] is the extent of the reference plane: every intermediate of the
//!   chain (grown matte, horizontal blur, vertical blur) reads the neutral value (0, or 1 for the
//!   inverted matte of the inner effects) beyond it, exactly as [`super::reference_matte`] reads
//!   beyond its plane. With `domain == layer` the result is bit-identical to the reference run on
//!   that plane including its edge truncation; with `domain` at least the layer grown by the effect
//!   support the result equals the uncut one and does not depend on the domain (tests
//!   `windowed_matte_is_bit_identical_to_the_reference_for_every_kind_and_split` and
//!   `padded_domain_equals_the_reference_and_ignores_extra_domain`). The window must lie inside the
//!   domain.
//! - The arithmetic is the reference arithmetic in the same order (f32 grown matte, f64 tap sums
//!   rounded to f32 after each blur pass), so the result is equal bit for bit, not within a
//!   tolerance.
//!
//! Bounded work and memory (V4-01/02): the geometry, parameters and sample domain are validated
//! first, then [`window_cost`] / [`layer_window_cost`] give the exact peak of owned bytes and the
//! work (inner-loop visits) BEFORE any allocation; a [`WindowLimits`] refusal is typed and happens
//! before the accessor is called, and every allocation is additionally charged against the running
//! aggregate (`tests/effect_window.rs` `budgets_are_charged_before_allocation_and_match_the_receipt`).
//! The receipt reports the measured peak, which equals the prediction in the tests.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    DropShadowOp, EffectError, EffectOp, EffectOrder, GlowSource, LayerParams,
    MAX_EFFECTS_PER_LAYER, StrokePosition, effect_blur_kernel, effect_support_px,
    knockout_coverage, shadow_offset, shadow_split, unit,
};
use crate::Rect;
use crate::blend_math::{
    Applicability, PixelSite, Rgba, composite_over, composite_over_extended, composite_over_fill,
    fill_inside_blend,
};
use crate::render_plan::SampleRange;
use std::fmt;
use std::ops::Range;

/// Largest window side in pixels (the byte budget bounds the area further).
pub const MAX_WINDOW_SIDE: u32 = 65_536;

/// Which budget of a [`WindowLimits`] was exceeded.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowBudget {
    Bytes,
    Work,
}

/// Typed failure of a windowed evaluation.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowError {
    /// Parameter, sample or compositing failure of the reference maths.
    Effect(EffectError),
    /// Window empty or above [`MAX_WINDOW_SIDE`], or a buffer length disagrees with its rectangle.
    WindowShape,
    /// The window is not inside the domain.
    OutsideDomain,
    /// The needed bytes or work exceed the limit (refused before the allocation or loop).
    OverBudget {
        budget: WindowBudget,
        needed: u64,
        limit: u64,
    },
    /// Coordinate or size arithmetic overflowed.
    Overflow,
    /// The allocator refused a charged buffer.
    AllocationFailed,
    /// The alpha accessor refused.
    Source,
    /// More effects than [`MAX_EFFECTS_PER_LAYER`].
    TooManyEffects,
    /// An internal geometry invariant failed (not expected from validated geometry).
    Geometry,
}
impl WindowError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Effect(e) => e.code(),
            Self::WindowShape => "window_shape",
            Self::OutsideDomain => "window_outside_domain",
            Self::OverBudget { .. } => "window_over_budget",
            Self::Overflow => "window_overflow",
            Self::AllocationFailed => "window_allocation_failed",
            Self::Source => "window_source_refused",
            Self::TooManyEffects => "window_too_many_effects",
            Self::Geometry => "window_geometry",
        }
    }
}
impl fmt::Display for WindowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Effect(e) => write!(f, "{e}"),
            _ => f.write_str(self.code()),
        }
    }
}
impl std::error::Error for WindowError {}
impl From<EffectError> for WindowError {
    fn from(e: EffectError) -> Self {
        Self::Effect(e)
    }
}

type WResult<T> = Result<T, WindowError>;

/// Reader of the layer SHAPE alpha (the alpha the effect matte is derived from).
///
/// `read_alpha` fills `out` (row-major, `rect.width * rect.height` samples in `[0, 1]`) for a
/// rectangle that lies inside [`WindowGeometry::layer`]. Today pigment calls it with single-row
/// rectangles clipped to the layer and the domain; an implementation must accept any rectangle
/// inside the layer. Samples outside `[0, 1]` or non-finite are refused as an out-of-domain effect
/// error.
pub trait AlphaSource {
    fn read_alpha(&self, rect: Rect, out: &mut [f32]) -> Result<(), WindowError>;
}
impl<F> AlphaSource for F
where
    F: Fn(Rect, &mut [f32]) -> Result<(), WindowError>,
{
    fn read_alpha(&self, rect: Rect, out: &mut [f32]) -> Result<(), WindowError> {
        self(rect, out)
    }
}

/// A borrowed row-major alpha plane covering `rect`, usable as an [`AlphaSource`].
#[derive(Clone, Copy, Debug)]
pub struct AlphaPlaneRef<'a> {
    rect: Rect,
    alpha: &'a [f32],
}
impl<'a> AlphaPlaneRef<'a> {
    pub fn new(rect: Rect, alpha: &'a [f32]) -> WResult<Self> {
        if alpha.len() != count(rect)? {
            return Err(WindowError::WindowShape);
        }
        Ok(Self { rect, alpha })
    }
}
impl AlphaSource for AlphaPlaneRef<'_> {
    fn read_alpha(&self, rect: Rect, out: &mut [f32]) -> WResult<()> {
        let (ex, ey) = ends(rect)?;
        let width = usize::try_from(rect.width).map_err(|_| WindowError::Overflow)?;
        for (row, y) in (rect.y..ey).enumerate() {
            let from = span(self.rect, y, rect.x, ex).map_err(|_| WindowError::Source)?;
            let start = row.checked_mul(width).ok_or(WindowError::Overflow)?;
            let end = start.checked_add(width).ok_or(WindowError::Overflow)?;
            let dst = out.get_mut(start..end).ok_or(WindowError::Source)?;
            dst.copy_from_slice(self.alpha.get(from).ok_or(WindowError::Source)?);
        }
        Ok(())
    }
}

/// Where the effect is defined and which part of it is wanted (all in layer pixel coordinates).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowGeometry {
    /// Rectangle in which the accessor has alpha; alpha beyond it is 0.
    pub layer: Rect,
    /// Extent of the reference plane: intermediates beyond it read the neutral value. Pass the
    /// layer grown by the effect support ([`WindowGeometry::uncut`]) for the uncut result.
    pub domain: Rect,
    /// The output window; must lie inside `domain`.
    pub window: Rect,
    /// Pixel whose coordinates are `(0, 0)` for the dissolve blend mode of [`render_layer_window`];
    /// the domain's top-left corner by default (the reference plane's origin).
    pub site_origin: (i64, i64),
}
impl WindowGeometry {
    pub const fn new(layer: Rect, domain: Rect, window: Rect) -> Self {
        Self {
            layer,
            domain,
            window,
            site_origin: (domain.x, domain.y),
        }
    }
    /// The reference plane itself: layer and domain are the same rectangle.
    pub const fn plane(plane: Rect, window: Rect) -> Self {
        Self::new(plane, plane, window)
    }
    /// The uncut geometry: the domain is the layer grown by `support_px` per side (use
    /// [`super::effect_support_px`] or [`stack_support_px`]).
    pub fn uncut(layer: Rect, support_px: u32, window: Rect) -> WResult<Self> {
        Ok(Self::new(layer, grow(layer, support_px)?, window))
    }
    pub const fn with_site_origin(mut self, origin: (i64, i64)) -> Self {
        self.site_origin = origin;
        self
    }
    fn check(&self) -> WResult<()> {
        let w = self.window;
        if w.width == 0 || w.height == 0 || w.width > MAX_WINDOW_SIDE || w.height > MAX_WINDOW_SIDE
        {
            return Err(WindowError::WindowShape);
        }
        ends(self.layer)?;
        ends(self.domain)?;
        if intersect(w, self.domain)? != Some(w) {
            return Err(WindowError::OutsideDomain);
        }
        Ok(())
    }
}

/// Limits for one windowed call: owned bytes at the peak and work (inner-loop visits).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowLimits {
    pub max_bytes: u64,
    pub max_work: u64,
}
impl WindowLimits {
    pub const fn new(max_bytes: u64, max_work: u64) -> Self {
        Self {
            max_bytes,
            max_work,
        }
    }
}

/// Predicted peak owned bytes and work of a call; the receipt equals it for every effect kind in
/// `tests/effect_window.rs` `budgets_are_charged_before_allocation_and_match_the_receipt`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowCost {
    pub peak_bytes: u64,
    pub work: u64,
}

/// Measured peak owned bytes (charged allocations alive at once, the returned result included)
/// and work of a finished call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowReceipt {
    pub peak_bytes: u64,
    pub work: u64,
}

/// The matte of one effect over `window` (coverage in `[0, 1]` before colour, mode and opacity),
/// row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowMatte {
    pub window: Rect,
    pub coverage: Vec<f32>,
    pub receipt: WindowReceipt,
}

// ---------------------------------------------------------------------------------------------
// Checked geometry helpers.

/// A colour channel in `range`: `[0, 1]` (unit) or any finite value (extended).
fn colour_in(range: SampleRange, v: f32) -> bool {
    if range == SampleRange::Extended {
        v.is_finite()
    } else {
        unit(v)
    }
}

fn add(a: i64, b: i64) -> WResult<i64> {
    a.checked_add(b).ok_or(WindowError::Overflow)
}
fn sub(a: i64, b: i64) -> WResult<i64> {
    a.checked_sub(b).ok_or(WindowError::Overflow)
}
fn addu(a: u64, b: u64) -> WResult<u64> {
    a.checked_add(b).ok_or(WindowError::Overflow)
}
fn mulu(a: u64, b: u64) -> WResult<u64> {
    a.checked_mul(b).ok_or(WindowError::Overflow)
}
fn usz(v: i64) -> WResult<usize> {
    usize::try_from(v).map_err(|_| WindowError::Overflow)
}
fn ends(r: Rect) -> WResult<(i64, i64)> {
    r.end().map_err(|_| WindowError::Overflow)
}
fn area(r: Rect) -> WResult<u64> {
    mulu(u64::from(r.width), u64::from(r.height))
}
fn count(r: Rect) -> WResult<usize> {
    usize::try_from(area(r)?).map_err(|_| WindowError::Overflow)
}
fn intersect(a: Rect, b: Rect) -> WResult<Option<Rect>> {
    a.intersect(b).map_err(|_| WindowError::Overflow)
}
/// `r` grown by `m` pixels on every side.
fn grow(r: Rect, m: u32) -> WResult<Rect> {
    let twice = m.checked_mul(2).ok_or(WindowError::Overflow)?;
    Ok(Rect {
        x: sub(r.x, i64::from(m))?,
        y: sub(r.y, i64::from(m))?,
        width: r.width.checked_add(twice).ok_or(WindowError::Overflow)?,
        height: r.height.checked_add(twice).ok_or(WindowError::Overflow)?,
    })
}
/// `r` moved by `(-dx, -dy)`: the source rectangle of a shift by `(dx, dy)`.
fn unshift(r: Rect, (dx, dy): (i32, i32)) -> WResult<Rect> {
    Ok(Rect {
        x: sub(r.x, i64::from(dx))?,
        y: sub(r.y, i64::from(dy))?,
        width: r.width,
        height: r.height,
    })
}
/// Index range of the samples `x0..x1` of row `y` in a row-major buffer covering `rect`.
fn span(rect: Rect, y: i64, x0: i64, x1: i64) -> WResult<Range<usize>> {
    let (ex, ey) = ends(rect)?;
    if y < rect.y || y >= ey || x0 < rect.x || x1 > ex || x1 < x0 {
        return Err(WindowError::Geometry);
    }
    let row = usz(sub(y, rect.y)?)?;
    let col = usz(sub(x0, rect.x)?)?;
    let len = usz(sub(x1, x0)?)?;
    let width = usize::try_from(rect.width).map_err(|_| WindowError::Overflow)?;
    let start = row
        .checked_mul(width)
        .and_then(|v| v.checked_add(col))
        .ok_or(WindowError::Overflow)?;
    let end = start.checked_add(len).ok_or(WindowError::Overflow)?;
    Ok(start..end)
}
fn square(v: u64) -> WResult<u64> {
    mulu(v, v)
}

// ---------------------------------------------------------------------------------------------
// Charged allocation.

struct Meter {
    live: u64,
    peak: u64,
    work: u64,
    limits: WindowLimits,
}
impl Meter {
    fn new(limits: WindowLimits) -> Self {
        Self {
            live: 0,
            peak: 0,
            work: 0,
            limits,
        }
    }
    fn reserve(&mut self, bytes: u64) -> WResult<()> {
        let live = addu(self.live, bytes)?;
        if live > self.limits.max_bytes {
            return Err(WindowError::OverBudget {
                budget: WindowBudget::Bytes,
                needed: live,
                limit: self.limits.max_bytes,
            });
        }
        self.live = live;
        self.peak = self.peak.max(live);
        Ok(())
    }
    fn release(&mut self, bytes: u64) {
        self.live = self.live.saturating_sub(bytes);
    }
    fn take(&mut self, n: usize, fill: f32) -> WResult<Vec<f32>> {
        let bytes = mulu(u64::try_from(n).map_err(|_| WindowError::Overflow)?, 4)?;
        self.reserve(bytes)?;
        let mut v: Vec<f32> = Vec::new();
        if v.try_reserve_exact(n).is_err() {
            self.release(bytes);
            return Err(WindowError::AllocationFailed);
        }
        v.resize(n, fill);
        Ok(v)
    }
    fn give(&mut self, v: Vec<f32>) {
        self.release(u64::try_from(v.len()).unwrap_or(u64::MAX).saturating_mul(4));
    }
    fn charge_work(&mut self, n: u64) -> WResult<()> {
        let work = addu(self.work, n)?;
        if work > self.limits.max_work {
            return Err(WindowError::OverBudget {
                budget: WindowBudget::Work,
                needed: work,
                limit: self.limits.max_work,
            });
        }
        self.work = work;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Prepared effect: parameters resolved once, cost derived from the same numbers.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Post {
    None,
    /// `m * (1 - alpha)`: drop shadow with the layer knocking it out.
    Knockout,
    /// `m * alpha`: inner shadow, inner glow from the edge.
    MulAlpha,
    /// `(1 - m) * alpha`: inner glow from the centre.
    OneMinusMulAlpha,
    /// `1 - m` (no alpha): the interior coverage of an inner glow from the centre (lane
    /// SMB-PS-ORACLE-PARITY-1).
    OneMinus,
}

struct ChainPlan {
    /// The source is `1 - alpha` (inner effects); beyond the layer it reads 1.
    inverted: bool,
    grown_px: f32,
    kd: u32,
    kernel: Vec<f32>,
    kb: u32,
    offset: (i32, i32),
    post: Post,
}
impl ChainPlan {
    fn new(
        inverted: bool,
        (grown_px, blur_px): (f32, f32),
        offset: (i32, i32),
        post: Post,
    ) -> WResult<Self> {
        let kernel = effect_blur_kernel(blur_px)?;
        let kb = u32::try_from(kernel.len() / 2).map_err(|_| WindowError::Overflow)?;
        let kd = if grown_px > 0.0 {
            grown_px.ceil() as u32
        } else {
            0
        };
        Ok(Self {
            inverted,
            grown_px,
            kd,
            kernel,
            kb,
            offset,
            post,
        })
    }
    /// The value of a transparent sample under the source transform (also the `outside` value).
    fn neutral(&self) -> f32 {
        if self.inverted { 1.0 } else { 0.0 }
    }
}

struct HalfPlan {
    w: f32,
    kd: u32,
    /// Outside half (`true`) dilates the region, inside half dilates its complement.
    outside: bool,
}

enum Plan {
    Overlay,
    /// The interior coverage of a colour overlay: 1 everywhere (lane SMB-PS-ORACLE-PARITY-1).
    Ones,
    Chain(ChainPlan),
    Stroke { halves: Vec<HalfPlan>, centre: bool },
    /// Satin and bevel (module `style`): the style function on the window grown by `support`,
    /// clipped to the domain (see [`run_local`]). `per_px` bounds the neighbourhood visits per
    /// grown-window pixel (satin: two blur passes + 4; bevel: `style::bevel_work_per_px`).
    Local {
        op: EffectOp,
        support: u32,
        per_px: u64,
        /// Satin's interior coverage: the style function without the final multiply by alpha.
        interior: bool,
    },
    /// Gradient overlay: alpha x stop opacity, with a colour per pixel (`true`: the interior
    /// coverage, the stop opacity alone).
    Gradient(super::GradientOverlayOp, bool),
    /// Pattern overlay (lane SMB-PS-ADJUST-STYLES-2): alpha x pattern alpha, with the pattern
    /// colour per pixel (`true`: the interior coverage, the pattern alpha alone).
    Pattern(super::PatternOverlayOp, bool),
}

/// Bytes per pixel of the grown window reserved for the style function's own buffers (satin keeps
/// at most four planes alive plus one blur pass buffer, bevel three; six is the bound charged).
const LOCAL_INTERNAL_BYTES: u64 = 24;

fn half(w: f32, outside: bool) -> HalfPlan {
    let kd = if w > 0.0 { w.ceil() as u32 } else { 0 };
    HalfPlan { w, kd, outside }
}

/// The plan of an interior effect's interior coverage ([`super::reference_interior_matte`]): the
/// effect's plan without the final multiply by the shape alpha.
fn prepare_interior(op: &EffectOp) -> WResult<Plan> {
    Ok(match prepare(op)? {
        Plan::Overlay => Plan::Ones,
        Plan::Gradient(o, _) => Plan::Gradient(o, true),
        Plan::Pattern(o, _) => Plan::Pattern(o, true),
        Plan::Local {
            op, support, per_px, ..
        } => Plan::Local {
            op,
            support,
            per_px,
            interior: true,
        },
        Plan::Chain(mut c) => {
            c.post = match c.post {
                Post::MulAlpha => Post::None,
                Post::OneMinusMulAlpha => Post::OneMinus,
                other => other,
            };
            Plan::Chain(c)
        }
        other => other,
    })
}

/// The plan an effect of a layer composition runs: the interior coverage for an interior effect.
fn prepare_composed(op: &EffectOp) -> WResult<Plan> {
    if op.is_interior() {
        prepare_interior(op)
    } else {
        prepare(op)
    }
}

fn prepare(op: &EffectOp) -> WResult<Plan> {
    op.validate()?;
    Ok(match op {
        EffectOp::ColourOverlay(_) => Plan::Overlay,
        EffectOp::GradientOverlay(o) => Plan::Gradient(*o, false),
        EffectOp::PatternOverlay(o) => Plan::Pattern(*o, false),
        EffectOp::Satin(_) | EffectOp::BevelEmboss(_) => {
            let size = match op {
                EffectOp::Satin(o) => o.size_px,
                EffectOp::BevelEmboss(o) => o.shape.size_px,
                _ => 0.0,
            };
            let kernel = effect_blur_kernel(size)?;
            Plan::Local {
                op: *op,
                support: effect_support_px(op)?,
                per_px: match op {
                    EffectOp::BevelEmboss(o) => super::style::bevel_work_per_px(o)?,
                    _ => addu(
                        mulu(u64::try_from(kernel.len()).map_err(|_| WindowError::Overflow)?, 2)?,
                        4,
                    )?,
                },
                interior: false,
            }
        }
        EffectOp::DropShadow(o) => Plan::Chain(ChainPlan::new(
            false,
            shadow_split(o.size_px, o.spread),
            shadow_offset(o.angle_deg, o.distance_px)?,
            if o.layer_knocks_out {
                Post::Knockout
            } else {
                Post::None
            },
        )?),
        EffectOp::OuterGlow(o) => Plan::Chain(ChainPlan::new(
            false,
            shadow_split(o.size_px, o.spread),
            (0, 0),
            Post::None,
        )?),
        EffectOp::InnerShadow(o) => Plan::Chain(ChainPlan::new(
            true,
            shadow_split(o.size_px, o.choke),
            shadow_offset(o.angle_deg, o.distance_px)?,
            Post::MulAlpha,
        )?),
        EffectOp::InnerGlow(o) => Plan::Chain(ChainPlan::new(
            true,
            shadow_split(o.size_px, o.choke),
            (0, 0),
            match o.source {
                GlowSource::Edge => Post::MulAlpha,
                GlowSource::Centre => Post::OneMinusMulAlpha,
            },
        )?),
        EffectOp::Stroke(o) => match o.position {
            StrokePosition::Outside => Plan::Stroke {
                halves: vec![half(o.size_px, true)],
                centre: false,
            },
            StrokePosition::Inside => Plan::Stroke {
                halves: vec![half(o.size_px, false)],
                centre: false,
            },
            StrokePosition::Centre => {
                let h = o.size_px / 2.0;
                Plan::Stroke {
                    halves: vec![half(h, true), half(h, false)],
                    centre: true,
                }
            }
        },
    })
}

#[derive(Clone, Copy)]
struct Cost {
    peak: u64,
    work: u64,
}

/// Bytes and work of one dilation stage reading `a0` samples and writing `a1` (kd > 0), counting
/// the coverage row.
fn dilate_cost(a0: u64, a1: u64, kd: u32) -> WResult<(u64, u64)> {
    let cov = mulu(addu(u64::from(kd), 1)?, 4)?;
    let bytes = addu(mulu(addu(a0, a1)?, 4)?, cov)?;
    let work = mulu(square(addu(mulu(u64::from(kd), 2)?, 1)?)?, a1)?;
    Ok((bytes, work))
}

impl ChainPlan {
    fn cost(&self, ww: u64, wh: u64) -> WResult<Cost> {
        let (kb, kd) = (u64::from(self.kb), u64::from(self.kd));
        let aw = mulu(ww, wh)?;
        let w1 = addu(ww, mulu(kb, 2)?)?;
        let h1 = addu(wh, mulu(kb, 2)?)?;
        let a1 = mulu(w1, h1)?;
        let a0 = mulu(addu(w1, mulu(kd, 2)?)?, addu(h1, mulu(kd, 2)?)?)?;
        let ah = mulu(ww, h1)?;
        let klen = addu(mulu(kb, 2)?, 1)?;
        let mut peak = mulu(a0, 4)?;
        let mut work = a0;
        if self.kd > 0 {
            let (b, w) = dilate_cost(a0, a1, self.kd)?;
            peak = peak.max(b);
            work = addu(work, w)?;
        }
        if self.kb > 0 {
            peak = peak.max(mulu(addu(a1, ah)?, 4)?);
            peak = peak.max(addu(mulu(addu(ah, aw)?, 4)?, mulu(ww, 8)?)?);
            work = addu(work, mulu(addu(ah, aw)?, klen)?)?;
        }
        if self.post == Post::OneMinus {
            work = addu(work, aw)?;
        } else if self.post != Post::None {
            peak = peak.max(mulu(aw, 8)?);
            work = addu(work, mulu(aw, 2)?)?;
        }
        peak = peak.max(mulu(aw, 4)?);
        Ok(Cost { peak, work })
    }
}

impl HalfPlan {
    fn cost(&self, ww: u64, wh: u64) -> WResult<Cost> {
        let aw = mulu(ww, wh)?;
        if self.w <= 0.0 {
            return Ok(Cost {
                peak: mulu(aw, 4)?,
                work: aw,
            });
        }
        let kd = u64::from(self.kd);
        let a0 = mulu(addu(ww, mulu(kd, 2)?)?, addu(wh, mulu(kd, 2)?)?)?;
        let (b, w) = dilate_cost(a0, aw, self.kd)?;
        let peak = mulu(a0, 4)?.max(b).max(mulu(aw, 8)?);
        let work = addu(addu(a0, w)?, mulu(aw, 2)?)?;
        Ok(Cost { peak, work })
    }
}

impl Plan {
    fn cost(&self, window: Rect) -> WResult<Cost> {
        let (ww, wh) = (u64::from(window.width), u64::from(window.height));
        let aw = mulu(ww, wh)?;
        match self {
            Self::Overlay | Self::Ones => Ok(Cost {
                peak: mulu(aw, 4)?,
                work: aw,
            }),
            Self::Chain(c) => c.cost(ww, wh),
            // Upper bound (equal when the grown window lies inside the domain): sub-plane alpha
            // (4) + internal buffers (LOCAL_INTERNAL_BYTES) per grown pixel, plus the window
            // result; work = read + two blur passes + four per-pixel stages + the crop.
            Self::Local { support, per_px, .. } => {
                let ag = area(grow(window, *support)?)?;
                Ok(Cost {
                    peak: addu(mulu(ag, addu(4, LOCAL_INTERNAL_BYTES)?)?, mulu(aw, 4)?)?,
                    work: addu(mulu(ag, addu(*per_px, 1)?)?, aw)?,
                })
            }
            // Matte (4) and the per-pixel colours (12); read, opacity and colour passes.
            Self::Gradient(..) | Self::Pattern(..) => Ok(Cost {
                peak: mulu(aw, 16)?,
                work: mulu(aw, 3)?,
            }),
            Self::Stroke { halves, centre } => {
                let mut costs = Vec::with_capacity(halves.len());
                for h in halves {
                    costs.push(h.cost(ww, wh)?);
                }
                match (costs.first(), costs.get(1)) {
                    (Some(a), Some(b)) if *centre => Ok(Cost {
                        peak: a
                            .peak
                            .max(addu(mulu(aw, 4)?, b.peak)?)
                            .max(mulu(aw, 8)?),
                        work: addu(addu(a.work, b.work)?, aw)?,
                    }),
                    (Some(a), None) => Ok(*a),
                    _ => Err(WindowError::Geometry),
                }
            }
        }
    }
}

fn check_cost(cost: Cost, limits: WindowLimits) -> WResult<()> {
    if cost.peak > limits.max_bytes {
        return Err(WindowError::OverBudget {
            budget: WindowBudget::Bytes,
            needed: cost.peak,
            limit: limits.max_bytes,
        });
    }
    if cost.work > limits.max_work {
        return Err(WindowError::OverBudget {
            budget: WindowBudget::Work,
            needed: cost.work,
            limit: limits.max_work,
        });
    }
    Ok(())
}

fn check_window(window: Rect) -> WResult<()> {
    if window.width == 0
        || window.height == 0
        || window.width > MAX_WINDOW_SIDE
        || window.height > MAX_WINDOW_SIDE
    {
        return Err(WindowError::WindowShape);
    }
    ends(window)?;
    Ok(())
}

/// Predicted peak bytes and work of [`window_matte`] for `op` over a window of this size. Validates
/// the window size and the effect parameters; allocates nothing but the blur kernel.
pub fn window_cost(op: &EffectOp, window: Rect) -> WResult<WindowCost> {
    check_window(window)?;
    let cost = prepare(op)?.cost(window)?;
    Ok(WindowCost {
        peak_bytes: cost.peak,
        work: cost.work,
    })
}

/// Predicted peak bytes and work of [`render_layer_window`] (result plane, content alpha and the
/// largest single matte alive at once).
pub fn layer_window_cost(effects: &[EffectOp], window: Rect) -> WResult<WindowCost> {
    check_window(window)?;
    compose_cost(effects, window).map(|c| WindowCost {
        peak_bytes: c.peak,
        work: c.work,
    })
}

fn compose_cost(effects: &[EffectOp], window: Rect) -> WResult<Cost> {
    if effects.len() > MAX_EFFECTS_PER_LAYER {
        return Err(WindowError::TooManyEffects);
    }
    let aw = area(window)?;
    // Result plane (rgb 12 + alpha 4 bytes per pixel) and the content alpha window (4).
    let base = mulu(aw, 20)?;
    let passes = u64::try_from(effects.len()).map_err(|_| WindowError::Overflow)?;
    let mut work = mulu(aw, addu(passes, 2)?)?;
    let mut matte_peak = 0u64;
    let mut interior_peak = 0u64;
    for op in effects {
        let c = prepare_composed(op)?.cost(window)?;
        if op.is_interior() {
            interior_peak = interior_peak.max(c.peak);
        } else {
            matte_peak = matte_peak.max(c.peak);
        }
        work = addu(work, c.work)?;
    }
    if effects.iter().any(EffectOp::is_interior) {
        // The interior unit (lane SMB-PS-ORACLE-PARITY-1): the copy of the result below (rgb 12 +
        // alpha 4 per pixel) lives through the content pass, with a window of ones (4), and
        // through every interior pass; copy, ones and mix are one visit per pixel each.
        let below = mulu(aw, 16)?;
        matte_peak = matte_peak
            .max(addu(below, mulu(aw, 4)?)?)
            .max(addu(below, interior_peak)?);
        work = addu(work, mulu(aw, 3)?)?;
    }
    Ok(Cost {
        peak: addu(base, matte_peak)?,
        work,
    })
}

/// The largest [`super::effect_support_px`] of the stack (0 for an empty stack): the margin a
/// shared domain needs so that every effect of the stack is uncut.
pub fn stack_support_px(effects: &[EffectOp]) -> Result<u32, EffectError> {
    let mut max = 0u32;
    for op in effects {
        max = max.max(effect_support_px(op)?);
    }
    Ok(max)
}

// ---------------------------------------------------------------------------------------------
// Pipeline stages.

/// Reads the alpha of `rect` inside the layer and the domain into `buf` (a buffer pre-filled with
/// the transform of a transparent sample) and applies `f` to every sample read.
fn read_alpha_into<S: AlphaSource + ?Sized>(
    src: &S,
    g: &WindowGeometry,
    rect: Rect,
    buf: &mut [f32],
    f: impl Fn(f32) -> f32,
) -> WResult<()> {
    let Some(clip) = intersect(rect, g.layer)? else {
        return Ok(());
    };
    let Some(clip) = intersect(clip, g.domain)? else {
        return Ok(());
    };
    let (cx1, cy1) = ends(clip)?;
    for y in clip.y..cy1 {
        let out = buf
            .get_mut(span(rect, y, clip.x, cx1)?)
            .ok_or(WindowError::Geometry)?;
        src.read_alpha(
            Rect {
                x: clip.x,
                y,
                width: clip.width,
                height: 1,
            },
            out,
        )?;
        for v in out.iter_mut() {
            if !unit(*v) {
                return Err(EffectError::OutOfDomain.into());
            }
            *v = f(*v);
        }
    }
    Ok(())
}

/// A window of ones, charged like [`alpha_window`] (the interior coverage of an overlay).
fn ones_window(g: &WindowGeometry, meter: &mut Meter) -> WResult<Vec<f32>> {
    let buf = meter.take(count(g.window)?, 1.0)?;
    meter.charge_work(area(g.window)?)?;
    Ok(buf)
}

/// The alpha over the window (0 beyond the layer and the domain).
fn alpha_window<S: AlphaSource + ?Sized>(
    src: &S,
    g: &WindowGeometry,
    meter: &mut Meter,
) -> WResult<Vec<f32>> {
    let mut buf = meter.take(count(g.window)?, 0.0)?;
    meter.charge_work(area(g.window)?)?;
    read_alpha_into(src, g, g.window, &mut buf, |a| a)?;
    Ok(buf)
}

/// `out(x) = max over y of src(y) * ramp(dist(x, y))` over `r1 ∩ domain`, the reference `dilate`
/// with radius `r` (`kd = ceil(r)`): `src` covers `r0 = r1` grown by `kd` and already holds the
/// neutral value beyond the domain; samples of `out` beyond the domain keep their pre-filled value.
#[allow(clippy::too_many_arguments)]
fn dilate_into(
    src: &[f32],
    r0: Rect,
    out: &mut [f32],
    r1: Rect,
    domain: Rect,
    r: f32,
    kd: u32,
    meter: &mut Meter,
) -> WResult<()> {
    let covs_len = usize::try_from(kd)
        .map_err(|_| WindowError::Overflow)?
        .checked_add(1)
        .ok_or(WindowError::Overflow)?;
    // Charged before the early exit so that the receipt also equals the prediction when the shifted
    // window leaves the domain (tests/effect_window.rs, third window of the budgets test).
    let cov_bytes = mulu(u64::try_from(covs_len).map_err(|_| WindowError::Overflow)?, 4)?;
    meter.reserve(cov_bytes)?;
    let Some(clip) = intersect(r1, domain)? else {
        meter.release(cov_bytes);
        return Ok(());
    };
    let (cx1, cy1) = ends(clip)?;
    for y in clip.y..cy1 {
        out.get_mut(span(r1, y, clip.x, cx1)?)
            .ok_or(WindowError::Geometry)?
            .fill(0.0);
    }
    let k = i64::from(kd);
    let mut cov = vec![0.0f32; covs_len];
    let lo = k.checked_neg().ok_or(WindowError::Overflow)?;
    for oy in lo..=k {
        let ay = oy.unsigned_abs();
        let mut any = false;
        for (ax, c) in cov.iter_mut().enumerate() {
            *c = if ax == 0 && oy == 0 {
                1.0
            } else {
                let ddx = (ax as f32 - 0.5).max(0.0);
                let ddy = (ay as f32 - 0.5).max(0.0);
                (r + 0.5 - ddx.hypot(ddy)).clamp(0.0, 1.0)
            };
            any |= *c > 0.0;
        }
        if !any {
            continue;
        }
        for y in clip.y..cy1 {
            let src_row = src
                .get(span(r0, add(y, oy)?, sub(clip.x, k)?, add(cx1, k)?)?)
                .ok_or(WindowError::Geometry)?;
            let out_row = out
                .get_mut(span(r1, y, clip.x, cx1)?)
                .ok_or(WindowError::Geometry)?;
            let width = out_row.len();
            for ox in lo..=k {
                let c = cov
                    .get(usize::try_from(ox.unsigned_abs()).map_err(|_| WindowError::Overflow)?)
                    .copied()
                    .ok_or(WindowError::Geometry)?;
                if c <= 0.0 {
                    continue;
                }
                let start = usz(add(ox, k)?)?;
                let end = start.checked_add(width).ok_or(WindowError::Overflow)?;
                let shifted = src_row.get(start..end).ok_or(WindowError::Geometry)?;
                for (b, s) in out_row.iter_mut().zip(shifted) {
                    *b = b.max(*s * c);
                }
            }
        }
    }
    meter.release(cov_bytes);
    Ok(())
}

/// Separable blur of `s1` (covering `r1` = `r2` grown by `kb`) into a buffer covering `r2`: the
/// horizontal pass over the rows `r1` needs, then the vertical pass, each tap sum in f64 in tap
/// order and rounded to f32 after the pass (the reference `blur_ext`). Samples beyond the domain
/// are `outside`. Takes `s1` so that it is released before the vertical pass allocates.
fn blur_into(
    s1: Vec<f32>,
    r1: Rect,
    kernel: &[f32],
    r2: Rect,
    domain: Rect,
    outside: f32,
    meter: &mut Meter,
) -> WResult<Vec<f32>> {
    let kb = i64::try_from(kernel.len() / 2).map_err(|_| WindowError::Overflow)?;
    let rh = Rect {
        x: r2.x,
        y: r1.y,
        width: r2.width,
        height: r1.height,
    };
    let mut h = meter.take(count(rh)?, outside)?;
    if let Some(clip) = intersect(rh, domain)? {
        let (cx1, cy1) = ends(clip)?;
        for y in clip.y..cy1 {
            let row_in = s1
                .get(span(r1, y, sub(clip.x, kb)?, add(cx1, kb)?)?)
                .ok_or(WindowError::Geometry)?;
            let out = h
                .get_mut(span(rh, y, clip.x, cx1)?)
                .ok_or(WindowError::Geometry)?;
            for (o, win) in out.iter_mut().zip(row_in.windows(kernel.len())) {
                *o = kernel
                    .iter()
                    .zip(win)
                    .fold(0.0f64, |acc, (t, v)| acc + f64::from(*t) * f64::from(*v))
                    as f32;
            }
        }
    }
    meter.give(s1);
    let mut s2 = meter.take(count(r2)?, outside)?;
    let acc_bytes = mulu(u64::from(r2.width), 8)?;
    meter.reserve(acc_bytes)?;
    let mut acc = vec![0.0f64; usize::try_from(r2.width).map_err(|_| WindowError::Overflow)?];
    if let Some(clip) = intersect(r2, domain)? {
        let (cx1, cy1) = ends(clip)?;
        let cols = usz(sub(cx1, clip.x)?)?;
        for y in clip.y..cy1 {
            let acc = acc.get_mut(..cols).ok_or(WindowError::Geometry)?;
            acc.fill(0.0);
            for (i, tap) in kernel.iter().enumerate() {
                let yy = add(
                    sub(y, kb)?,
                    i64::try_from(i).map_err(|_| WindowError::Overflow)?,
                )?;
                let row = h
                    .get(span(rh, yy, clip.x, cx1)?)
                    .ok_or(WindowError::Geometry)?;
                for (a, v) in acc.iter_mut().zip(row) {
                    *a += f64::from(*tap) * f64::from(*v);
                }
            }
            let out = s2
                .get_mut(span(r2, y, clip.x, cx1)?)
                .ok_or(WindowError::Geometry)?;
            for (o, a) in out.iter_mut().zip(acc.iter()) {
                *o = *a as f32;
            }
        }
    }
    meter.release(acc_bytes);
    meter.give(h);
    Ok(s2)
}

/// Drop shadow, outer glow, inner shadow and inner glow: source (alpha or `1 - alpha`), grow by a
/// hard disc, blur, shift by the offset (the result rectangle is the window moved by `-offset`, so
/// its local indices are the window's), then the post step.
fn run_chain<S: AlphaSource + ?Sized>(
    src: &S,
    g: &WindowGeometry,
    c: &ChainPlan,
    meter: &mut Meter,
) -> WResult<Vec<f32>> {
    let neutral = c.neutral();
    let r2 = unshift(g.window, c.offset)?;
    let r1 = grow(r2, c.kb)?;
    let r0 = grow(r1, c.kd)?;
    let mut s0 = meter.take(count(r0)?, neutral)?;
    meter.charge_work(area(r0)?)?;
    let inverted = c.inverted;
    read_alpha_into(src, g, r0, &mut s0, |a| if inverted { 1.0 - a } else { a })?;
    let s1 = if c.kd > 0 {
        meter.charge_work(mulu(
            square(addu(mulu(u64::from(c.kd), 2)?, 1)?)?,
            area(r1)?,
        )?)?;
        let mut s1 = meter.take(count(r1)?, neutral)?;
        dilate_into(&s0, r0, &mut s1, r1, g.domain, c.grown_px, c.kd, meter)?;
        meter.give(s0);
        s1
    } else {
        s0
    };
    let mut s2 = if c.kb > 0 {
        let klen = u64::try_from(c.kernel.len()).map_err(|_| WindowError::Overflow)?;
        let ah = mulu(u64::from(r2.width), u64::from(r1.height))?;
        meter.charge_work(mulu(addu(ah, area(r2)?)?, klen)?)?;
        blur_into(s1, r1, &c.kernel, r2, g.domain, neutral, meter)?
    } else {
        s1
    };
    if c.post == Post::OneMinus {
        meter.charge_work(area(g.window)?)?;
        for m in s2.iter_mut() {
            *m = 1.0 - *m;
        }
    } else if c.post != Post::None {
        // `alpha_window` charges the read; this charges the per-sample multiply.
        meter.charge_work(area(g.window)?)?;
        let a = alpha_window(src, g, meter)?;
        for (m, a) in s2.iter_mut().zip(&a) {
            *m = match c.post {
                Post::Knockout => *m * (1.0 - *a),
                Post::MulAlpha => *m * *a,
                Post::OneMinusMulAlpha => (1.0 - *m) * *a,
                Post::None | Post::OneMinus => *m,
            };
        }
        meter.give(a);
    }
    Ok(s2)
}

/// One stroke half over the window (reference `stroke_half`).
fn run_half<S: AlphaSource + ?Sized>(
    src: &S,
    g: &WindowGeometry,
    h: &HalfPlan,
    meter: &mut Meter,
) -> WResult<Vec<f32>> {
    let aw = count(g.window)?;
    if h.w <= 0.0 {
        meter.charge_work(area(g.window)?)?;
        return meter.take(aw, 0.0);
    }
    let neutral = if h.outside { 0.0 } else { 1.0 };
    let r0 = grow(g.window, h.kd)?;
    let mut s0 = meter.take(count(r0)?, neutral)?;
    meter.charge_work(area(r0)?)?;
    let outside_half = h.outside;
    read_alpha_into(src, g, r0, &mut s0, |a| {
        let region = if a >= 0.5 { 1.0 } else { 0.0 };
        if outside_half { region } else { 1.0 - region }
    })?;
    meter.charge_work(mulu(
        square(addu(mulu(u64::from(h.kd), 2)?, 1)?)?,
        area(g.window)?,
    )?)?;
    let mut d = meter.take(aw, neutral)?;
    dilate_into(&s0, r0, &mut d, g.window, g.domain, h.w, h.kd, meter)?;
    meter.give(s0);
    let a = alpha_window(src, g, meter)?;
    meter.charge_work(area(g.window)?)?;
    for (dv, av) in d.iter_mut().zip(&a) {
        *dv = if outside_half {
            *dv * (1.0 - *av)
        } else {
            *dv * *av
        };
    }
    meter.give(a);
    Ok(d)
}

/// The layer bounds as `(x, y, width, height)` for the gradient geometry.
fn layer_bounds(g: &WindowGeometry) -> (i64, i64, u32, u32) {
    (g.layer.x, g.layer.y, g.layer.width, g.layer.height)
}

/// Per-pixel gradient colours over the window (12 bytes per pixel, charged).
fn gradient_colours(
    g: &WindowGeometry,
    o: &super::GradientOverlayOp,
    meter: &mut Meter,
) -> WResult<Vec<[f32; 3]>> {
    let n = count(g.window)?;
    meter.reserve(mulu(area(g.window)?, 12)?)?;
    meter.charge_work(area(g.window)?)?;
    let mut out: Vec<[f32; 3]> = Vec::new();
    out.try_reserve_exact(n)
        .map_err(|_| WindowError::AllocationFailed)?;
    let bounds = layer_bounds(g);
    let (ex, ey) = ends(g.window)?;
    for y in g.window.y..ey {
        for x in g.window.x..ex {
            out.push(super::style::gradient_at(o, x, y, bounds).0);
        }
    }
    Ok(out)
}

/// Satin and bevel over the window: the style function of module `style` runs on the window grown
/// by the effect support and clipped to the domain (alpha beyond the layer is 0, beyond the
/// sub-plane every stage reads its neutral 0), then the window is cut out. Every window sample
/// depends only on alpha within the support, so the result equals the reference on the domain
/// bit for bit (same function, same arithmetic order).
#[allow(clippy::too_many_arguments)]
fn run_local<S: AlphaSource + ?Sized>(
    src: &S,
    g: &WindowGeometry,
    op: &EffectOp,
    support: u32,
    per_px: u64,
    interior: bool,
    meter: &mut Meter,
) -> WResult<Vec<f32>> {
    let grown = grow(g.window, support)?;
    let sub = intersect(grown, g.domain)?.ok_or(WindowError::Geometry)?;
    let ag = area(grown)?;
    let mut alpha = meter.take(count(sub)?, 0.0)?;
    meter.charge_work(ag)?;
    read_alpha_into(src, g, sub, &mut alpha, |a| a)?;
    let internal = mulu(ag, LOCAL_INTERNAL_BYTES)?;
    meter.reserve(internal)?;
    meter.charge_work(mulu(ag, per_px)?)?;
    let grid = super::Grid {
        w: i64::from(sub.width),
        h: i64::from(sub.height),
    };
    let m = match op {
        EffectOp::Satin(o) => super::style::satin_matte_by(&alpha, grid, o, !interior)?,
        EffectOp::BevelEmboss(o) => super::style::bevel_matte(&alpha, grid, o)?,
        _ => return Err(WindowError::Geometry),
    };
    let mut out = meter.take(count(g.window)?, 0.0)?;
    meter.charge_work(area(g.window)?)?;
    let (ex, ey) = ends(g.window)?;
    for y in g.window.y..ey {
        let from = m
            .get(span(sub, y, g.window.x, ex)?)
            .ok_or(WindowError::Geometry)?;
        out.get_mut(span(g.window, y, g.window.x, ex)?)
            .ok_or(WindowError::Geometry)?
            .copy_from_slice(from);
    }
    drop(m);
    meter.release(internal);
    meter.give(alpha);
    Ok(out)
}

/// Per-pixel pattern colours over the window (12 bytes per pixel, charged).
fn pattern_colours(
    g: &WindowGeometry,
    o: &super::PatternOverlayOp,
    pixels: &[[f32; 4]],
    meter: &mut Meter,
) -> WResult<Vec<[f32; 3]>> {
    let n = count(g.window)?;
    meter.reserve(mulu(area(g.window)?, 12)?)?;
    meter.charge_work(area(g.window)?)?;
    let mut out: Vec<[f32; 3]> = Vec::new();
    out.try_reserve_exact(n)
        .map_err(|_| WindowError::AllocationFailed)?;
    let (ex, ey) = ends(g.window)?;
    for y in g.window.y..ey {
        for x in g.window.x..ex {
            let p = o.sample_at(x, y, g.layer, pixels).ok_or(WindowError::Geometry)?;
            out.push([p[0], p[1], p[2]]);
        }
    }
    Ok(out)
}

fn run_matte<S: AlphaSource + ?Sized>(
    src: &S,
    g: &WindowGeometry,
    plan: &Plan,
    patterns: &dyn super::PatternPixels,
    meter: &mut Meter,
) -> WResult<Vec<f32>> {
    match plan {
        Plan::Pattern(o, interior) => {
            let pixels = super::style::pattern_pixels(o, patterns)?;
            let mut m = if *interior { ones_window(g, meter)? } else { alpha_window(src, g, meter)? };
            meter.charge_work(area(g.window)?)?;
            let (ex, ey) = ends(g.window)?;
            let mut i = 0usize;
            for y in g.window.y..ey {
                for x in g.window.x..ex {
                    let p = o.sample_at(x, y, g.layer, pixels).ok_or(WindowError::Geometry)?;
                    if let Some(v) = m.get_mut(i) {
                        *v *= p[3];
                    }
                    i = i.checked_add(1).ok_or(WindowError::Overflow)?;
                }
            }
            Ok(m)
        }
        Plan::Overlay => alpha_window(src, g, meter),
        Plan::Ones => ones_window(g, meter),
        Plan::Local {
            op,
            support,
            per_px,
            interior,
        } => run_local(src, g, op, *support, *per_px, *interior, meter),
        Plan::Gradient(o, interior) => {
            let mut m = if *interior { ones_window(g, meter)? } else { alpha_window(src, g, meter)? };
            meter.charge_work(area(g.window)?)?;
            let bounds = layer_bounds(g);
            let (ex, ey) = ends(g.window)?;
            let mut i = 0usize;
            for y in g.window.y..ey {
                for x in g.window.x..ex {
                    let (_, opacity) = super::style::gradient_at(o, x, y, bounds);
                    if let Some(v) = m.get_mut(i) {
                        *v *= opacity;
                    }
                    i = i.checked_add(1).ok_or(WindowError::Overflow)?;
                }
            }
            Ok(m)
        }
        Plan::Chain(c) => run_chain(src, g, c, meter),
        Plan::Stroke { halves, centre } => {
            let mut it = halves.iter();
            let first = it.next().ok_or(WindowError::Geometry)?;
            let mut out = run_half(src, g, first, meter)?;
            if *centre {
                let second = it.next().ok_or(WindowError::Geometry)?;
                let inn = run_half(src, g, second, meter)?;
                meter.charge_work(area(g.window)?)?;
                for (a, b) in out.iter_mut().zip(&inn) {
                    *a = (*a + *b).min(1.0);
                }
                meter.give(inn);
            }
            Ok(out)
        }
    }
}

/// The matte of one effect for `geometry.window`, bit-identical to [`super::reference_matte`] (see
/// the module doc for the edge policy). Refuses before reading any alpha when the predicted bytes
/// or work exceed `limits`.
pub fn window_matte<S: AlphaSource + ?Sized>(
    src: &S,
    geometry: &WindowGeometry,
    op: &EffectOp,
    limits: WindowLimits,
) -> WResult<WindowMatte> {
    window_matte_with(src, geometry, op, limits, &super::NoPatterns)
}

/// [`window_matte`] with the pattern pixels a pattern overlay reads (lane SMB-PS-ADJUST-STYLES-2).
pub fn window_matte_with<S: AlphaSource + ?Sized>(
    src: &S,
    geometry: &WindowGeometry,
    op: &EffectOp,
    limits: WindowLimits,
    patterns: &dyn super::PatternPixels,
) -> WResult<WindowMatte> {
    geometry.check()?;
    let plan = prepare(op)?;
    check_cost(plan.cost(geometry.window)?, limits)?;
    let mut meter = Meter::new(limits);
    let coverage = run_matte(src, geometry, &plan, patterns, &mut meter)?;
    Ok(WindowMatte {
        window: geometry.window,
        coverage,
        receipt: WindowReceipt {
            peak_bytes: meter.peak,
            work: meter.work,
        },
    })
}

#[cfg(test)]
mod meter_tests {
    use super::*;

    /// The running aggregate is checked on every allocation (V4-02 `aggregate_over_budget`): each
    /// buffer fits alone, the sum does not, and releasing makes room again.
    #[test]
    fn running_aggregate_is_charged_per_allocation() {
        let mut m = Meter::new(WindowLimits::new(100, 10));
        let a = m.take(20, 0.0).unwrap();
        assert_eq!(m.live, 80);
        assert_eq!(
            m.take(10, 0.0).unwrap_err(),
            WindowError::OverBudget {
                budget: WindowBudget::Bytes,
                needed: 120,
                limit: 100
            }
        );
        assert_eq!((m.live, m.peak), (80, 80), "a refused take charges nothing");
        m.give(a);
        assert_eq!(m.live, 0);
        let b = m.take(25, 0.0).unwrap();
        assert_eq!((m.live, m.peak), (100, 100));
        m.give(b);
        m.charge_work(10).unwrap();
        assert_eq!(
            m.charge_work(1).unwrap_err(),
            WindowError::OverBudget {
                budget: WindowBudget::Work,
                needed: 11,
                limit: 10
            }
        );
        assert_eq!(m.work, 10);
        let _kept = m.take(1, 0.0).unwrap();
        assert_eq!(
            m.reserve(u64::MAX).unwrap_err(),
            WindowError::Overflow,
            "checked add, not a wrap"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Composition of a layer stack over a window.

/// Straight-RGBA samples of a window, row-major, every sample in `[0, 1]`.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowPlane {
    rect: Rect,
    rgb: Vec<[f32; 3]>,
    alpha: Vec<f32>,
}
impl WindowPlane {
    pub fn new(rect: Rect, rgb: Vec<[f32; 3]>, alpha: Vec<f32>) -> WResult<Self> {
        Self::new_in(SampleRange::Unit, rect, rgb, alpha)
    }
    /// [`WindowPlane::new`] in `range` (extended: any finite colour; alpha stays `[0, 1]`).
    pub fn new_in(
        range: SampleRange,
        rect: Rect,
        rgb: Vec<[f32; 3]>,
        alpha: Vec<f32>,
    ) -> WResult<Self> {
        check_window(rect)?;
        let n = count(rect)?;
        if rgb.len() != n || alpha.len() != n {
            return Err(WindowError::WindowShape);
        }
        if !rgb.iter().all(|p| p.iter().all(|c| colour_in(range, *c))) || !alpha.iter().all(|a| unit(*a)) {
            return Err(EffectError::OutOfDomain.into());
        }
        Ok(Self { rect, rgb, alpha })
    }
    pub const fn rect(&self) -> Rect {
        self.rect
    }
    pub fn rgb(&self) -> &[[f32; 3]] {
        &self.rgb
    }
    pub fn alpha(&self) -> &[f32] {
        &self.alpha
    }
}

/// The composed window and the measured cost.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerWindow {
    pub plane: WindowPlane,
    pub receipt: WindowReceipt,
}

enum Colours<'a> {
    Uniform([f32; 3]),
    Pixels(&'a [[f32; 3]]),
}

/// The content pass's fill split (lane SMB-PS-ORACLE-PARITY-1): `Some((opacity, fill))` when the
/// layer's mode is one of Photoshop's special eight and its fill is below 1 (pigment
/// `composite_over_fill`, as the reference composes the content), `None` for the product fold.
pub(super) fn content_fill(layer: LayerParams) -> Option<(f32, f32)> {
    (layer.fill_opacity < 1.0 && fill_inside_blend(layer.mode)).then_some((layer.opacity, layer.fill_opacity))
}

/// One source-over pass of the whole window (the reference composes pixel by pixel; pixels are
/// independent, so a pass per effect gives the same bits with one matte alive at a time).
#[allow(clippy::too_many_arguments)]
fn pass(
    rgb: &mut [[f32; 3]],
    alpha: &mut [f32],
    g: &WindowGeometry,
    mode: crate::blend_math::StudioBlendMode,
    opacity: f32,
    colours: &Colours<'_>,
    alphas: &[f32],
    clamp: bool,
    range: SampleRange,
    fill: Option<(f32, f32)>,
    meter: &mut Meter,
) -> WResult<()> {
    meter.charge_work(area(g.window)?)?;
    let width = i64::from(g.window.width);
    let x_base = sub(g.window.x, g.site_origin.0)?;
    let y_base = sub(g.window.y, g.site_origin.1)?;
    let (mut lx, mut ly) = (0i64, 0i64);
    for (i, (c, a)) in rgb.iter_mut().zip(alpha.iter_mut()).enumerate() {
        let m = *alphas.get(i).ok_or(WindowError::Geometry)?;
        let colour = match colours {
            Colours::Uniform(u) => *u,
            Colours::Pixels(p) => *p.get(i).ok_or(WindowError::Geometry)?,
        };
        let site = PixelSite {
            x: add(x_base, lx)?,
            y: add(y_base, ly)?,
            seed: 0,
        };
        let src = Rgba::new(colour, if clamp { m.clamp(0.0, 1.0) } else { m });
        let px = if range == SampleRange::Extended {
            composite_over_extended(mode, Rgba::new(*c, *a), src, opacity, site)
        } else if let Some((unfilled, f)) = fill {
            composite_over_fill(mode, Rgba::new(*c, *a), src, unfilled, f, site)
        } else {
            composite_over(mode, Rgba::new(*c, *a), src, opacity, site)
        }
        .map_err(|e| WindowError::Effect(EffectError::Blend(e)))?;
        *c = px.rgb;
        *a = px.a;
        lx = add(lx, 1)?;
        if lx == width {
            lx = 0;
            ly = add(ly, 1)?;
        }
    }
    Ok(())
}

/// The composition of [`super::render_layer_reference`] for `geometry.window`: every
/// [`EffectOrder::BehindContent`] effect in list order, the content (`content_rgb` with the shape
/// alpha read from `src`) at `opacity * fill`, then every [`EffectOrder::OverContent`] effect in
/// list order, over `backdrop` (which must cover the window). Bit-identical to the reference
/// restricted to the window; dissolve noise uses the coordinates relative to
/// [`WindowGeometry::site_origin`].
pub fn render_layer_window<S: AlphaSource + ?Sized>(
    src: &S,
    geometry: &WindowGeometry,
    content_rgb: &[[f32; 3]],
    backdrop: &WindowPlane,
    layer: LayerParams,
    effects: &[EffectOp],
    limits: WindowLimits,
) -> WResult<LayerWindow> {
    render_layer_window_in(
        SampleRange::Unit,
        src,
        geometry,
        content_rgb,
        backdrop,
        layer,
        effects,
        limits,
    )
}

/// [`render_layer_window`] in `range` (lane SMB-PIGMENT-EXTENDED-RANGE-2). Extended: the content
/// and backdrop colours may be any finite value, every pass composites with
/// `composite_over_extended` (the effect mattes are alpha maths and do not depend on the colour
/// range; the effect colours are authored unit values); the layer mode and every effect mode must
/// be `blend_math::extended_defined` (a typed `Blend(Unsupported)` otherwise).
#[allow(clippy::too_many_arguments)]
pub fn render_layer_window_in<S: AlphaSource + ?Sized>(
    range: SampleRange,
    src: &S,
    geometry: &WindowGeometry,
    content_rgb: &[[f32; 3]],
    backdrop: &WindowPlane,
    layer: LayerParams,
    effects: &[EffectOp],
    limits: WindowLimits,
) -> WResult<LayerWindow> {
    render_layer_window_patterned_in(
        range,
        src,
        geometry,
        content_rgb,
        backdrop,
        layer,
        effects,
        limits,
        &super::NoPatterns,
    )
}

/// [`render_layer_window_in`] with the pattern pixels its pattern overlays read (lane
/// SMB-PS-ADJUST-STYLES-2; the pattern is anchored at `geometry.layer`, the windowed geometry of
/// the gradient overlay too).
#[allow(clippy::too_many_arguments)]
pub fn render_layer_window_patterned_in<S: AlphaSource + ?Sized>(
    range: SampleRange,
    src: &S,
    geometry: &WindowGeometry,
    content_rgb: &[[f32; 3]],
    backdrop: &WindowPlane,
    layer: LayerParams,
    effects: &[EffectOp],
    limits: WindowLimits,
    patterns: &dyn super::PatternPixels,
) -> WResult<LayerWindow> {
    geometry.check()?;
    let window = geometry.window;
    let n = count(window)?;
    if backdrop.rect != window || content_rgb.len() != n {
        return Err(WindowError::WindowShape);
    }
    if !content_rgb.iter().all(|p| p.iter().all(|c| colour_in(range, *c))) {
        return Err(EffectError::OutOfDomain.into());
    }
    if !unit(layer.opacity) || !unit(layer.fill_opacity) {
        return Err(EffectError::OutOfDomain.into());
    }
    if layer.mode.facts().applicability != Applicability::Layer {
        return Err(EffectError::ModeNotApplicable(layer.mode).into());
    }
    let cost = compose_cost(effects, window)?;
    check_cost(cost, limits)?;
    let plans = effects.iter().map(prepare_composed).collect::<WResult<Vec<_>>>()?;

    let mut meter = Meter::new(limits);
    let content_alpha = alpha_window(src, geometry, &mut meter)?;
    meter.reserve(mulu(area(window)?, 12)?)?;
    meter.reserve(mulu(area(window)?, 4)?)?;
    let mut rgb: Vec<[f32; 3]> = Vec::new();
    rgb.try_reserve_exact(n)
        .map_err(|_| WindowError::AllocationFailed)?;
    rgb.extend_from_slice(&backdrop.rgb);
    let mut alpha: Vec<f32> = Vec::new();
    alpha
        .try_reserve_exact(n)
        .map_err(|_| WindowError::AllocationFailed)?;
    alpha.extend_from_slice(&backdrop.alpha);

    let interior = effects.iter().any(EffectOp::is_interior);
    // Behind-content effects, the content (with the interior unit when the stack has interior
    // effects), then the other over-content effects; each group in list order.
    for stage in 0..3u8 {
        if stage == 1 {
            if interior {
                interior_unit(
                    (&mut rgb, &mut alpha),
                    InteriorInputs { src, geometry, content_rgb, content_alpha: &content_alpha, patterns },
                    layer,
                    effects,
                    &plans,
                    range,
                    &mut meter,
                )?;
            } else {
                pass(
                    &mut rgb,
                    &mut alpha,
                    geometry,
                    layer.mode,
                    layer.opacity * layer.fill_opacity,
                    &Colours::Pixels(content_rgb),
                    &content_alpha,
                    false,
                    range,
                    content_fill(layer),
                    &mut meter,
                )?;
            }
            continue;
        }
        for (op, plan) in effects.iter().zip(&plans) {
            let wanted = if stage == 0 {
                op.placement() == EffectOrder::BehindContent
            } else {
                op.placement() == EffectOrder::OverContent && !op.is_interior()
            };
            if !wanted {
                continue;
            }
            let mut matte = run_matte(src, geometry, plan, patterns, &mut meter)?;
            if let EffectOp::DropShadow(DropShadowOp { layer_knocks_out: true, .. }) = op {
                // the reference's composition rule (`knockout_coverage`); the multiply is part
                // of this pass's charged work
                let of = layer.opacity * layer.fill_opacity;
                for (m, a) in matte.iter_mut().zip(&content_alpha) {
                    *m = knockout_coverage(m.clamp(0.0, 1.0), *a, of);
                }
            }
            let per_pixel = match plan {
                Plan::Gradient(o, _) => Some(gradient_colours(geometry, o, &mut meter)?),
                Plan::Pattern(o, _) => {
                    let pixels = super::style::pattern_pixels(o, patterns)?;
                    Some(pattern_colours(geometry, o, pixels, &mut meter)?)
                }
                _ => None,
            };
            let colours = match &per_pixel {
                Some(c) => Colours::Pixels(c),
                None => Colours::Uniform(op.colour()),
            };
            pass(
                &mut rgb,
                &mut alpha,
                geometry,
                op.mode(),
                op.opacity() * layer.opacity,
                &colours,
                &matte,
                true,
                range,
                None,
                &mut meter,
            )?;
            if per_pixel.is_some() {
                meter.release(mulu(area(window)?, 12)?);
            }
            meter.give(matte);
        }
    }
    meter.give(content_alpha);
    Ok(LayerWindow {
        plane: WindowPlane {
            rect: window,
            rgb,
            alpha,
        },
        receipt: WindowReceipt {
            peak_bytes: meter.peak,
            work: meter.work,
        },
    })
}

/// What the interior unit reads.
struct InteriorInputs<'a, S: AlphaSource + ?Sized> {
    src: &'a S,
    geometry: &'a WindowGeometry,
    content_rgb: &'a [[f32; 3]],
    content_alpha: &'a [f32],
    patterns: &'a dyn super::PatternPixels,
}

/// The interior unit of [`render_layer_window`] (lane SMB-PS-ORACLE-PARITY-1, the reference's
/// `interior_mix` rule): the result below is kept, the content composites over it as if it
/// covered every pixel, the interior effects composite over that with their interior coverage in
/// list order, and every pixel is mixed back by the shape alpha ([`super::interior_mix`]).
#[allow(clippy::too_many_arguments)]
fn interior_unit<S: AlphaSource + ?Sized>(
    (rgb, alpha): (&mut Vec<[f32; 3]>, &mut Vec<f32>),
    inp: InteriorInputs<'_, S>,
    layer: LayerParams,
    effects: &[EffectOp],
    plans: &[Plan],
    range: SampleRange,
    meter: &mut Meter,
) -> WResult<()> {
    let g = inp.geometry;
    let aw = area(g.window)?;
    let n = count(g.window)?;
    meter.reserve(mulu(aw, 16)?)?;
    meter.charge_work(aw)?;
    let below_rgb = rgb.clone();
    let below_alpha = alpha.clone();
    let ones = meter.take(n, 1.0)?;
    meter.charge_work(aw)?;
    pass(
        rgb,
        alpha,
        g,
        layer.mode,
        layer.opacity * layer.fill_opacity,
        &Colours::Pixels(inp.content_rgb),
        &ones,
        false,
        range,
        content_fill(layer),
        meter,
    )?;
    meter.give(ones);
    for (op, plan) in effects.iter().zip(plans) {
        if !op.is_interior() {
            continue;
        }
        let matte = run_matte(inp.src, g, plan, inp.patterns, meter)?;
        let per_pixel = match plan {
            Plan::Gradient(o, _) => Some(gradient_colours(g, o, meter)?),
            Plan::Pattern(o, _) => {
                let pixels = super::style::pattern_pixels(o, inp.patterns)?;
                Some(pattern_colours(g, o, pixels, meter)?)
            }
            _ => None,
        };
        let colours = match &per_pixel {
            Some(c) => Colours::Pixels(c),
            None => Colours::Uniform(op.colour()),
        };
        pass(
            rgb,
            alpha,
            g,
            op.mode(),
            op.opacity() * layer.opacity,
            &colours,
            &matte,
            true,
            range,
            None,
            meter,
        )?;
        if per_pixel.is_some() {
            meter.release(mulu(aw, 12)?);
        }
        meter.give(matte);
    }
    meter.charge_work(aw)?;
    let unit = range != SampleRange::Extended;
    for (i, (c, a)) in rgb.iter_mut().zip(alpha.iter_mut()).enumerate() {
        let shape = *inp.content_alpha.get(i).ok_or(WindowError::Geometry)?;
        let below = Rgba::new(
            *below_rgb.get(i).ok_or(WindowError::Geometry)?,
            *below_alpha.get(i).ok_or(WindowError::Geometry)?,
        );
        let mixed = super::interior_mix(below, Rgba::new(*c, *a), shape, unit);
        *c = mixed.rgb;
        *a = mixed.a;
    }
    drop(below_rgb);
    drop(below_alpha);
    meter.release(mulu(aw, 16)?);
    Ok(())
}
