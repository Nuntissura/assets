//! Layer styles of lane SMB-PS-ADJUST-STYLES (STU-RAS-040, STU-RAS-157, STU-FX-132): satin, bevel &
//! emboss (smooth technique) and gradient overlay, as [`super::EffectOp`] variants. Same contract as
//! the rest of [`super`]: scalar maths on straight `[0, 1]` values, typed refusals for non-finite
//! or out-of-range parameters, class `Approximate` (Photoshop publishes none of these formulas;
//! every reading below is UNVERIFIED, D5). The per-pixel stage functions here are the ONE
//! definition used by both the reference ([`super::reference_matte`]) and the windowed evaluator
//! ([`super::window`]), which runs them on the window grown by the effect support and clipped to
//! the domain, so the two agree bit for bit (`tests/layer_styles.rs`).
//!
//! - Satin: `B = Gaussian(alpha, size)`, `m = |B(x - o) - B(x + o)|` with `o` the whole-pixel
//!   shadow offset of `(angle, distance)` ([`super::shadow_offset`]), `1 - m` when inverted, times
//!   the alpha (drawn inside the shape). Krita's satin also combines two offset blurred copies of
//!   the shape (behaviour reference, GPL, no code copied); the exact Photoshop combination is
//!   UNVERIFIED.
//! - Bevel & emboss, smooth: height `h = depth / 100 * size * Gaussian(alpha, size)`, gradient by
//!   central differences (beyond the plane the blurred alpha reads 0), surface normal
//!   `N = normalize(-dh/dx, -dh/dy, 1)` (`direction down` negates the height), light
//!   `L = (cos alt cos angle, -cos alt sin angle, sin alt)` (angle counter-clockwise from +x, y
//!   down, Photoshop's dial convention), shading `s = N . L` against the flat `s0 = sin alt`:
//!   highlight `clamp((s - s0) / (1 - s0))`, shadow `clamp((s0 - s) / s0)`. Region: inner bevel x
//!   alpha, outer bevel x (1 - alpha), emboss everywhere, pillow emboss inside with the height and
//!   outside with the negated height. The highlight and the shadow are two ops ([`BevelPart`]) so
//!   each keeps its own colour, mode and opacity. Chisel techniques, stroke emboss and soften are
//!   refused ([`super::EffectFeature`]).
//! - Gradient overlay: colour and stop opacity of a gradient over the LAYER BOUNDS (the reference
//!   plane, or [`super::window::WindowGeometry::layer`]); the matte is alpha x stop opacity. Styles
//!   linear, radial, angle, reflected, diamond; the geometry readings (box-projection length for
//!   linear, half the larger side for radial and diamond) are Studio's, UNVERIFIED.
#![deny(clippy::arithmetic_side_effects)]
use super::{EffectError, EffectFeature, Grid, MAX_EFFECT_SIZE_PX, blur_ext, shift_ext};
use crate::blend_math::StudioBlendMode;
use crate::coverage_math::{feather_kernel, feather_support};
use serde::Serialize;

/// Gradient stops one overlay op carries (the op stays `Copy`; longer gradients are refused at
/// lowering).
pub const MAX_GRADIENT_STOPS: usize = 16;

fn unit(v: f32) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}

/// Satin. Composited over the content, inside the shape.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct SatinOp {
    pub colour: [f32; 3],
    pub mode: StudioBlendMode,
    pub opacity: f32,
    pub angle_deg: f32,
    pub distance_px: f32,
    pub size_px: f32,
    pub invert: bool,
}
impl SatinOp {
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        colour: [f32; 3],
        mode: StudioBlendMode,
        opacity: f32,
        angle_deg: f32,
        distance_px: f32,
        size_px: f32,
        invert: bool,
    ) -> Self {
        Self {
            colour,
            mode,
            opacity,
            angle_deg,
            distance_px,
            size_px,
            invert,
        }
    }
}

/// Bevel style (STU-RAS-157 `style`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum BevelStyle {
    InnerBevel,
    OuterBevel,
    Emboss,
    PillowEmboss,
    StrokeEmboss,
}
/// Bevel technique (STU-RAS-157 `technique`); only `Smooth` renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum BevelTechnique {
    Smooth,
    ChiselHard,
    ChiselSoft,
}
/// Which half of the bevel shading an op draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum BevelPart {
    Highlight,
    Shadow,
}

/// The shape of a bevel, shared by its highlight and shadow ops. `depth` is the STU-RAS-157 percent
/// `[1, 1000]`; `angle_deg` the absolute light angle (Global Light resolved by the builder),
/// `altitude_deg` `[0, 90]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BevelShape {
    pub style: BevelStyle,
    pub technique: BevelTechnique,
    pub up: bool,
    pub depth: f32,
    pub size_px: f32,
    pub soften_px: f32,
    pub angle_deg: f32,
    pub altitude_deg: f32,
}
impl BevelShape {
    /// Inner bevel, smooth, up, depth 100, size 5, no soften, light 120 deg at 30 deg (the
    /// STU-FX-132 defaults).
    pub const DEFAULT: Self = Self {
        style: BevelStyle::InnerBevel,
        technique: BevelTechnique::Smooth,
        up: true,
        depth: 100.0,
        size_px: 5.0,
        soften_px: 0.0,
        angle_deg: 120.0,
        altitude_deg: 30.0,
    };
}

/// One half of a bevel & emboss.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BevelEmbossOp {
    pub part: BevelPart,
    pub colour: [f32; 3],
    pub mode: StudioBlendMode,
    pub opacity: f32,
    pub shape: BevelShape,
}
impl BevelEmbossOp {
    pub const fn new(
        part: BevelPart,
        colour: [f32; 3],
        mode: StudioBlendMode,
        opacity: f32,
        shape: BevelShape,
    ) -> Self {
        Self {
            part,
            colour,
            mode,
            opacity,
            shape,
        }
    }
}

/// Gradient geometry style (STU-FX-132 `Style`: linear, radial, angle, reflected, diamond).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum GradientStyle {
    Linear,
    Radial,
    Angle,
    Reflected,
    Diamond,
}

/// One gradient stop: position and opacity `[0, 1]`, straight RGB `[0, 1]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct GradientStopOp {
    pub position: f32,
    pub colour: [f32; 3],
    pub opacity: f32,
}
impl GradientStopOp {
    pub const UNUSED: Self = Self {
        position: 0.0,
        colour: [0.0; 3],
        opacity: 0.0,
    };
}

/// Gradient overlay: the first `stop_count` entries of `stops` (2..=[`MAX_GRADIENT_STOPS`],
/// positions non-decreasing), style, angle (degrees `[-180, 180]`), scale (fraction `[0.1, 1.5]`,
/// STU-FX-132 10..150 %), reverse, and the centre offset as a fraction of the layer size.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct GradientOverlayOp {
    pub mode: StudioBlendMode,
    pub opacity: f32,
    pub style: GradientStyle,
    pub angle_deg: f32,
    pub scale: f32,
    pub reverse: bool,
    pub offset: [f32; 2],
    pub stops: [GradientStopOp; MAX_GRADIENT_STOPS],
    pub stop_count: u8,
}
impl GradientOverlayOp {
    /// An overlay from a stop list (refused when longer than [`MAX_GRADIENT_STOPS`]).
    pub fn new(
        mode: StudioBlendMode,
        opacity: f32,
        style: GradientStyle,
        angle_deg: f32,
        stops: &[GradientStopOp],
    ) -> Result<Self, EffectError> {
        if stops.len() > MAX_GRADIENT_STOPS {
            return Err(EffectError::Unsupported(EffectFeature::GradientStops));
        }
        let mut array = [GradientStopOp::UNUSED; MAX_GRADIENT_STOPS];
        for (slot, s) in array.iter_mut().zip(stops) {
            *slot = *s;
        }
        Ok(Self {
            mode,
            opacity,
            style,
            angle_deg,
            scale: 1.0,
            reverse: false,
            offset: [0.0; 2],
            stops: array,
            stop_count: u8::try_from(stops.len()).map_err(|_| EffectError::Overflow)?,
        })
    }
    pub const fn with_scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }
    pub const fn with_reverse(mut self, reverse: bool) -> Self {
        self.reverse = reverse;
        self
    }
    pub const fn with_offset(mut self, offset: [f32; 2]) -> Self {
        self.offset = offset;
        self
    }
    /// The used stops.
    pub fn used_stops(&self) -> &[GradientStopOp] {
        self.stops
            .get(..usize::from(self.stop_count))
            .unwrap_or(&[])
    }
}

fn angle_ok(v: f32) -> Result<(), EffectError> {
    if !v.is_finite() {
        Err(EffectError::OutOfDomain)
    } else if !(-180.0..=180.0).contains(&v) {
        Err(EffectError::AngleOutOfRange)
    } else {
        Ok(())
    }
}
fn size_ok(v: f32) -> Result<(), EffectError> {
    if !v.is_finite() || v < 0.0 {
        Err(EffectError::OutOfDomain)
    } else if v > MAX_EFFECT_SIZE_PX {
        Err(EffectError::SizeOutOfRange)
    } else {
        Ok(())
    }
}

pub(super) fn validate_satin(o: &SatinOp) -> Result<(), EffectError> {
    angle_ok(o.angle_deg)?;
    super::shadow_offset(o.angle_deg, o.distance_px)?;
    size_ok(o.size_px)
}

pub(super) fn validate_bevel(o: &BevelEmbossOp) -> Result<(), EffectError> {
    let s = &o.shape;
    angle_ok(s.angle_deg)?;
    size_ok(s.size_px)?;
    if !s.altitude_deg.is_finite() || !(0.0..=90.0).contains(&s.altitude_deg) {
        return Err(EffectError::OutOfDomain);
    }
    if !s.depth.is_finite() || !(1.0..=1000.0).contains(&s.depth) {
        return Err(EffectError::OutOfDomain);
    }
    if !s.soften_px.is_finite() || s.soften_px < 0.0 {
        return Err(EffectError::OutOfDomain);
    }
    if s.technique != BevelTechnique::Smooth {
        return Err(EffectError::Unsupported(EffectFeature::BevelChisel));
    }
    if s.style == BevelStyle::StrokeEmboss {
        return Err(EffectError::Unsupported(EffectFeature::BevelStrokeEmboss));
    }
    if s.soften_px > 0.0 {
        return Err(EffectError::Unsupported(EffectFeature::BevelSoften));
    }
    Ok(())
}

pub(super) fn validate_gradient(o: &GradientOverlayOp) -> Result<(), EffectError> {
    angle_ok(o.angle_deg)?;
    if !o.scale.is_finite() || !(0.1..=1.5).contains(&o.scale) || !o.offset.iter().all(|v| v.is_finite() && v.abs() <= 1.0) {
        return Err(EffectError::OutOfDomain);
    }
    let n = usize::from(o.stop_count);
    if !(2..=MAX_GRADIENT_STOPS).contains(&n) {
        return Err(EffectError::OutOfDomain);
    }
    let stops = o.used_stops();
    if !stops
        .iter()
        .all(|s| unit(s.position) && unit(s.opacity) && s.colour.iter().all(|c| unit(*c)))
        || !stops.windows(2).all(|w| match w {
            [a, b] => a.position <= b.position,
            _ => true,
        })
    {
        return Err(EffectError::OutOfDomain);
    }
    Ok(())
}

/// Support (pixels read around the layer) of the three kinds.
pub(super) fn satin_support(o: &SatinOp) -> Result<u32, EffectError> {
    let blur = feather_support(o.size_px).map_err(|_| EffectError::SizeOutOfRange)?;
    let (dx, dy) = super::shadow_offset(o.angle_deg, o.distance_px)?;
    dx.unsigned_abs()
        .max(dy.unsigned_abs())
        .checked_add(blur)
        .ok_or(EffectError::Overflow)
}
pub(super) fn bevel_support(o: &BevelEmbossOp) -> Result<u32, EffectError> {
    feather_support(o.shape.size_px)
        .map_err(|_| EffectError::SizeOutOfRange)?
        .checked_add(1)
        .ok_or(EffectError::Overflow)
}

/// Satin matte over a plane `g` with shape alpha `alpha` (beyond the plane: transparent).
pub(super) fn satin_matte(alpha: &[f32], g: Grid, o: &SatinOp) -> Result<Vec<f32>, EffectError> {
    let kernel = feather_kernel(o.size_px).map_err(|_| EffectError::SizeOutOfRange)?;
    let blurred = blur_ext(alpha, g, &kernel, 0.0)?;
    let (dx, dy) = super::shadow_offset(o.angle_deg, o.distance_px)?;
    let plus = shift_ext(&blurred, g, dx, dy, 0.0)?;
    let minus = shift_ext(
        &blurred,
        g,
        dx.checked_neg().ok_or(EffectError::Overflow)?,
        dy.checked_neg().ok_or(EffectError::Overflow)?,
        0.0,
    )?;
    Ok(plus
        .iter()
        .zip(&minus)
        .zip(alpha)
        .map(|((p, m), a)| {
            let d = (p - m).abs().clamp(0.0, 1.0);
            (if o.invert { 1.0 - d } else { d }) * a
        })
        .collect())
}

/// Bevel matte (the op's part) over a plane `g`.
pub(super) fn bevel_matte(alpha: &[f32], g: Grid, o: &BevelEmbossOp) -> Result<Vec<f32>, EffectError> {
    let s = &o.shape;
    let kernel = feather_kernel(s.size_px).map_err(|_| EffectError::SizeOutOfRange)?;
    let h = blur_ext(alpha, g, &kernel, 0.0)?;
    let scale = s.depth / 100.0 * s.size_px * if s.up { 1.0 } else { -1.0 };
    let (sa, ca) = f64::from(s.angle_deg).to_radians().sin_cos();
    let (sl, cl) = f64::from(s.altitude_deg).to_radians().sin_cos();
    let light = [cl * ca, -cl * sa, sl];
    let flat = sl;
    let part = |gx: f64, gy: f64, k: f64| -> f64 {
        let n = [-k * gx, -k * gy, 1.0];
        let len = (n[0] * n[0] + n[1] * n[1] + 1.0).sqrt();
        let shade = (n[0] * light[0] + n[1] * light[1] + n[2] * light[2]) / len;
        match o.part {
            BevelPart::Highlight => {
                if flat >= 1.0 {
                    0.0
                } else {
                    ((shade - flat) / (1.0 - flat)).clamp(0.0, 1.0)
                }
            }
            BevelPart::Shadow => {
                if flat <= 0.0 {
                    (-shade).clamp(0.0, 1.0)
                } else {
                    ((flat - shade) / flat).clamp(0.0, 1.0)
                }
            }
        }
    };
    let mut out = vec![0.0f32; g.len()?];
    for y in 0..g.h {
        for x in 0..g.w {
            let at = |xx: i64, yy: i64| f64::from(g.get(&h, xx, yy).unwrap_or(0.0));
            let xm = x.checked_sub(1).ok_or(EffectError::Overflow)?;
            let xp = x.checked_add(1).ok_or(EffectError::Overflow)?;
            let ym = y.checked_sub(1).ok_or(EffectError::Overflow)?;
            let yp = y.checked_add(1).ok_or(EffectError::Overflow)?;
            let gx = (at(xp, y) - at(xm, y)) * 0.5;
            let gy = (at(x, yp) - at(x, ym)) * 0.5;
            let a = f64::from(g.get(alpha, x, y).unwrap_or(0.0));
            let k = f64::from(scale);
            let v = match s.style {
                BevelStyle::InnerBevel => part(gx, gy, k) * a,
                BevelStyle::OuterBevel => part(gx, gy, k) * (1.0 - a),
                BevelStyle::Emboss => part(gx, gy, k),
                BevelStyle::PillowEmboss => part(gx, gy, k) * a + part(gx, gy, -k) * (1.0 - a),
                BevelStyle::StrokeEmboss => 0.0,
            };
            if let Some(slot) = g.idx(x, y).and_then(|i| out.get_mut(i)) {
                *slot = v.clamp(0.0, 1.0) as f32;
            }
        }
    }
    Ok(out)
}

/// The gradient parameter `t` in `[0, 1]` at the centre of pixel `(x, y)` for layer bounds
/// `(lx, ly, lw, lh)` (all in the same pixel coordinates).
fn gradient_t(o: &GradientOverlayOp, x: i64, y: i64, bounds: (i64, i64, u32, u32)) -> f64 {
    let (lx, ly, lw, lh) = bounds;
    let w = f64::from(lw);
    let hgt = f64::from(lh);
    let cx = lx as f64 + w * (0.5 + f64::from(o.offset[0]));
    let cy = ly as f64 + hgt * (0.5 + f64::from(o.offset[1]));
    let px = x as f64 + 0.5 - cx;
    let py = y as f64 + 0.5 - cy;
    let (sa, ca) = f64::from(o.angle_deg).to_radians().sin_cos();
    // Direction of the gradient in image coordinates (y down).
    let along = px * ca - py * sa;
    let across = px * sa + py * ca;
    let scale = f64::from(o.scale);
    let t = match o.style {
        GradientStyle::Linear | GradientStyle::Reflected => {
            let len = (w * ca.abs() + hgt * sa.abs()) * scale;
            let lin = if len > 0.0 { along / len + 0.5 } else { 0.5 };
            if o.style == GradientStyle::Reflected {
                ((lin - 0.5).abs() * 2.0).clamp(0.0, 1.0)
            } else {
                lin
            }
        }
        GradientStyle::Radial => {
            let r = 0.5 * w.max(hgt) * scale;
            if r > 0.0 { (px * px + py * py).sqrt() / r } else { 0.0 }
        }
        GradientStyle::Diamond => {
            let r = 0.5 * w.max(hgt) * scale;
            if r > 0.0 { (along.abs() + across.abs()) / r } else { 0.0 }
        }
        GradientStyle::Angle => {
            // Counter-clockwise angle from the gradient direction, one turn = 1.
            let a = (-across).atan2(along);
            a.rem_euclid(std::f64::consts::TAU) / std::f64::consts::TAU
        }
    };
    let t = t.clamp(0.0, 1.0);
    if o.reverse { 1.0 - t } else { t }
}

/// Colour and stop opacity of the gradient at `t`: the first stop at or below its position, the
/// last at or above, linear interpolation between the enclosing stops otherwise.
fn gradient_sample(o: &GradientOverlayOp, t: f64) -> ([f32; 3], f32) {
    let stops = o.used_stops();
    let t = t as f32;
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
        return ([0.0; 3], 0.0);
    };
    if t <= first.position {
        return (first.colour, first.opacity);
    }
    if t >= last.position {
        return (last.colour, last.opacity);
    }
    for w in stops.windows(2) {
        let [a, b] = w else { continue };
        if t >= a.position && t < b.position {
            let span = b.position - a.position;
            if span <= 0.0 {
                return (b.colour, b.opacity);
            }
            let f = (t - a.position) / span;
            let lerp = |p: f32, q: f32| p + (q - p) * f;
            return (
                [
                    lerp(a.colour[0], b.colour[0]),
                    lerp(a.colour[1], b.colour[1]),
                    lerp(a.colour[2], b.colour[2]),
                ],
                lerp(a.opacity, b.opacity),
            );
        }
    }
    (last.colour, last.opacity)
}

/// Gradient colour and stop opacity at pixel `(x, y)`.
pub(super) fn gradient_at(
    o: &GradientOverlayOp,
    x: i64,
    y: i64,
    bounds: (i64, i64, u32, u32),
) -> ([f32; 3], f32) {
    gradient_sample(o, gradient_t(o, x, y, bounds))
}
