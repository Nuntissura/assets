//! Layer styles of lane SMB-PS-ADJUST-STYLES (STU-RAS-040, STU-RAS-157, STU-FX-132): satin, bevel &
//! emboss (smooth technique) and gradient overlay, as [`super::EffectOp`] variants. Same contract as
//! the rest of [`super`]: scalar maths on straight `[0, 1]` values, typed refusals for non-finite
//! or out-of-range parameters, class `Approximate` (Photoshop publishes none of these formulas;
//! every reading below is UNVERIFIED, D5). The per-pixel stage functions here are the ONE
//! definition used by both the reference ([`super::reference_matte`]) and the windowed evaluator
//! ([`super::window`]), which runs them on the window grown by the effect support and clipped to
//! the domain, so the two agree bit for bit (`tests/layer_styles.rs`).
//!
//! - Satin: `B = tent(alpha, size)` (the effect blur [`super::effect_blur_kernel`], measured on the
//!   D5 oracle for shadows and glows; for satin UNVERIFIED), `m = |B(x - o) - B(x + o)|` with `o` the whole-pixel
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
//!   each keeps its own colour, mode and opacity.
//! - Bevel chisel hard (lane SMB-PS-ADJUST-STYLES-2): the height field is the signed distance to
//!   the shape edge instead of the blurred alpha: with the region `alpha >= 0.5` (beyond the plane:
//!   outside), `s = min(d_in - 0.5, size)` inside and `s = -min(d_out - 0.5, size)` outside, `d` the
//!   centre distance to the nearest pixel of the other side within `ceil(size + 0.5)` (none found:
//!   the cap), field `s / size` (the slope is `depth / 100`, constant across the bevel: the faceted
//!   chisel look; beyond the plane the field reads -1); size 0 is flat. Chisel soft: the chisel-hard
//!   field blurred by a Gaussian of `size / 4`. Both use the smooth shading and region rules.
//! - Soften: the part matte (after the region rule) blurred by the effect tent kernel of
//!   `soften_px` ([`super::effect_blur_kernel`]; Adobe: soften blurs the results of shading; the
//!   kernel is the one measured for shadows and glows, for soften UNVERIFIED).
//! - Stroke emboss: the emboss of the layer's stroke ([`BevelStroke`], resolved by the builder from
//!   the stroke effect on the same layer): the field is taken from the union `max(alpha, stroke)`
//!   and the region is the stroke matte (pigment's stroke rule); without a stroke the matte is empty
//!   (Adobe: stroke emboss shows nothing without a stroke). Chisel, soften and stroke-emboss
//!   readings are Studio's, UNVERIFIED (D5).
//! - Gradient overlay: colour and stop opacity of a gradient over the LAYER BOUNDS (the reference
//!   plane, or [`super::window::WindowGeometry::layer`]); the matte is alpha x stop opacity. Styles
//!   linear, radial, angle, reflected, diamond; the geometry readings (box-projection length for
//!   linear, half the larger side for radial and diamond) are Studio's, UNVERIFIED.
#![deny(clippy::arithmetic_side_effects)]
use super::{
    EffectError, EffectFeature, Grid, MAX_EFFECT_SIZE_PX, blur_ext, effect_blur_kernel,
    effect_blur_support, shift_ext,
};
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
/// Bevel technique (STU-RAS-157 `technique`).
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

/// The stroke a stroke-emboss bevel embosses: size and position of the stroke effect on the same
/// layer (the builder resolves it; pigment's stroke rule draws its matte).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BevelStroke {
    pub size_px: f32,
    pub position: super::StrokePosition,
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
    /// The embossed stroke of [`BevelStyle::StrokeEmboss`] (`None`: no stroke, empty matte); not
    /// read by the other styles.
    pub stroke: Option<BevelStroke>,
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
        stroke: None,
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
    /// Colour and stop opacity of the gradient at the centre of pixel `(x, y)` for the layer bounds
    /// `layer` (the one definition the matte and the colour use; public so a backend computes the
    /// per-pixel colours from the owner, lane SMB-PS-ADJUST-STYLES-2).
    pub fn sample_at(&self, x: i64, y: i64, layer: crate::Rect) -> ([f32; 3], f32) {
        gradient_at(self, x, y, (layer.x, layer.y, layer.width, layer.height))
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
    size_ok(s.soften_px)?;
    if let Some(stroke) = s.stroke {
        size_ok(stroke.size_px)?;
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
    let blur = effect_blur_support(o.size_px)?;
    let (dx, dy) = super::shadow_offset(o.angle_deg, o.distance_px)?;
    dx.unsigned_abs()
        .max(dy.unsigned_abs())
        .checked_add(blur)
        .ok_or(EffectError::Overflow)
}
/// Radius of the chisel distance search: `ceil(size + 0.5)` (a distance beyond it is capped).
fn chisel_radius(size_px: f32) -> u32 {
    (size_px + 0.5).ceil() as u32
}

/// Support of the stroke a stroke emboss reads (the stroke rule's distance window).
fn stroke_support(s: &BevelShape) -> u32 {
    match (s.style, s.stroke) {
        (BevelStyle::StrokeEmboss, Some(st)) => match st.position {
            super::StrokePosition::Centre => (st.size_px / 2.0).ceil() as u32,
            _ => st.size_px.ceil() as u32,
        },
        _ => 0,
    }
}

/// Support of the height field alone (before the central difference).
fn field_support(s: &BevelShape) -> Result<u32, EffectError> {
    let blur = |v: f32| feather_support(v).map_err(|_| EffectError::SizeOutOfRange);
    match s.technique {
        BevelTechnique::Smooth => blur(s.size_px),
        BevelTechnique::ChiselHard => Ok(chisel_radius(s.size_px)),
        BevelTechnique::ChiselSoft => chisel_radius(s.size_px)
            .checked_add(blur(s.size_px / 4.0)?)
            .ok_or(EffectError::Overflow),
    }
}

/// Stroke window + field + 1 (central difference) + soften blur.
pub(super) fn bevel_support(o: &BevelEmbossOp) -> Result<u32, EffectError> {
    let s = &o.shape;
    let soften = effect_blur_support(s.soften_px)?;
    stroke_support(s)
        .checked_add(field_support(s)?)
        .and_then(|v| v.checked_add(1))
        .and_then(|v| v.checked_add(soften))
        .ok_or(EffectError::Overflow)
}

/// Upper bound of the per-pixel neighbourhood visits of a bevel over its grown window (the
/// windowed evaluator charges `area x this` before reading any alpha).
pub(super) fn bevel_work_per_px(o: &BevelEmbossOp) -> Result<u64, EffectError> {
    let s = &o.shape;
    let taps = |v: f32| -> Result<u64, EffectError> {
        u64::try_from(feather_kernel(v).map_err(|_| EffectError::SizeOutOfRange)?.len())
            .map_err(|_| EffectError::Overflow)
    };
    let square = |k: u32| -> Result<u64, EffectError> {
        let side = u64::from(k)
            .checked_mul(2)
            .and_then(|v| v.checked_add(1))
            .ok_or(EffectError::Overflow)?;
        side.checked_mul(side).ok_or(EffectError::Overflow)
    };
    let twice = |v: u64| v.checked_mul(2).ok_or(EffectError::Overflow);
    let field = match s.technique {
        BevelTechnique::Smooth => twice(taps(s.size_px)?)?,
        BevelTechnique::ChiselHard => square(chisel_radius(s.size_px))?,
        BevelTechnique::ChiselSoft => square(chisel_radius(s.size_px))?
            .checked_add(twice(taps(s.size_px / 4.0)?)?)
            .ok_or(EffectError::Overflow)?,
    };
    // 4: the gradient, shading and copy visits (the smooth bevel's charge before this lane).
    let stroke = match stroke_support(s) {
        0 => 0,
        k => twice(square(k)?)?.checked_add(1).ok_or(EffectError::Overflow)?,
    };
    let soften = if s.soften_px > 0.0 {
        twice(
            u64::try_from(effect_blur_kernel(s.soften_px)?.len())
                .map_err(|_| EffectError::Overflow)?,
        )?
    } else {
        0
    };
    [field, stroke, soften, 4]
        .iter()
        .try_fold(0u64, |acc, v| acc.checked_add(*v))
        .ok_or(EffectError::Overflow)
}

/// Satin matte over a plane `g` with shape alpha `alpha` (beyond the plane: transparent).
pub(super) fn satin_matte(alpha: &[f32], g: Grid, o: &SatinOp) -> Result<Vec<f32>, EffectError> {
    satin_matte_by(alpha, g, o, true)
}

/// [`satin_matte`] with the final multiply by the alpha optional (`times_alpha = false`: the
/// interior coverage of lane SMB-PS-ORACLE-PARITY-1, see [`super::reference_interior_matte`]).
pub(super) fn satin_matte_by(
    alpha: &[f32],
    g: Grid,
    o: &SatinOp,
    times_alpha: bool,
) -> Result<Vec<f32>, EffectError> {
    let kernel = effect_blur_kernel(o.size_px)?;
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
            let v = if o.invert { 1.0 - d } else { d };
            if times_alpha { v * a } else { v }
        })
        .collect())
}

/// Chisel-hard field `s / size` of `shape` (beyond the plane: outside; see the module doc).
fn chisel_field(shape: &[f32], g: Grid, size_px: f32) -> Result<Vec<f32>, EffectError> {
    let mut out = vec![0.0f32; g.len()?];
    if size_px <= 0.0 {
        return Ok(out);
    }
    // The work (area x (2k + 1)^2) is charged by the caller: the reference budget in
    // `reference_matte`, the windowed meter in `window::run_local` (`bevel_work_per_px`).
    let k = i64::from(chisel_radius(size_px));
    let lo = k.checked_neg().ok_or(EffectError::Overflow)?;
    let size = f64::from(size_px);
    let inside_at = |x: i64, y: i64| g.get(shape, x, y).unwrap_or(0.0) >= 0.5;
    for y in 0..g.h {
        for x in 0..g.w {
            let inside = inside_at(x, y);
            let mut best = f64::INFINITY;
            for oy in lo..=k {
                for ox in lo..=k {
                    let sx = x.checked_add(ox).ok_or(EffectError::Overflow)?;
                    let sy = y.checked_add(oy).ok_or(EffectError::Overflow)?;
                    if inside_at(sx, sy) != inside {
                        best = best.min((ox as f64).hypot(oy as f64));
                    }
                }
            }
            let d = (best - 0.5).min(size);
            let signed = if inside { d } else { -d };
            let v = signed / size;
            if let Some(slot) = g.idx(x, y).and_then(|i| out.get_mut(i)) {
                *slot = v as f32;
            }
        }
    }
    Ok(out)
}

/// The stroke matte a stroke emboss embosses (pigment's stroke rule, `super::stroke_half`).
fn stroke_matte(alpha: &[f32], g: Grid, stroke: &BevelStroke) -> Result<Vec<f32>, EffectError> {
    let half_of = |w: f32, outside: bool| {
        super::stroke_half_by(alpha, g, w, outside, super::dilate_unbudgeted)
    };
    Ok(match stroke.position {
        super::StrokePosition::Outside => half_of(stroke.size_px, true)?,
        super::StrokePosition::Inside => half_of(stroke.size_px, false)?,
        super::StrokePosition::Centre => {
            let w = stroke.size_px / 2.0;
            let out = half_of(w, true)?;
            let inn = half_of(w, false)?;
            out.iter().zip(&inn).map(|(a, b)| (a + b).min(1.0)).collect()
        }
    })
}

/// Bevel matte (the op's part) over a plane `g`.
pub(super) fn bevel_matte(alpha: &[f32], g: Grid, o: &BevelEmbossOp) -> Result<Vec<f32>, EffectError> {
    let s = &o.shape;
    // Stroke emboss: the field comes from the union of the shape and its stroke, the region is the
    // stroke matte; without a stroke there is nothing to emboss.
    let stroke = match (s.style, s.stroke) {
        (BevelStyle::StrokeEmboss, Some(st)) => Some(stroke_matte(alpha, g, &st)?),
        (BevelStyle::StrokeEmboss, None) => return Ok(vec![0.0; g.len()?]),
        _ => None,
    };
    let union: Option<Vec<f32>> = stroke
        .as_ref()
        .map(|st| alpha.iter().zip(st).map(|(a, b)| a.max(*b)).collect());
    let shape = union.as_deref().unwrap_or(alpha);
    // The height field and its value beyond the plane (far outside the shape).
    let (h, beyond) = match s.technique {
        BevelTechnique::Smooth => {
            let kernel = feather_kernel(s.size_px).map_err(|_| EffectError::SizeOutOfRange)?;
            (blur_ext(shape, g, &kernel, 0.0)?, 0.0f64)
        }
        BevelTechnique::ChiselHard => (chisel_field(shape, g, s.size_px)?, -1.0),
        BevelTechnique::ChiselSoft => {
            let hard = chisel_field(shape, g, s.size_px)?;
            let kernel = feather_kernel(s.size_px / 4.0).map_err(|_| EffectError::SizeOutOfRange)?;
            (blur_ext(&hard, g, &kernel, -1.0)?, -1.0)
        }
    };
    let beyond = if s.size_px <= 0.0 { 0.0 } else { beyond };
    drop(union);
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
            let at = |xx: i64, yy: i64| g.get(&h, xx, yy).map_or(beyond, f64::from);
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
                BevelStyle::StrokeEmboss => {
                    let st = g.idx(x, y).and_then(|i| stroke.as_ref()?.get(i)).copied();
                    part(gx, gy, k) * f64::from(st.unwrap_or(0.0))
                }
            };
            if let Some(slot) = g.idx(x, y).and_then(|i| out.get_mut(i)) {
                *slot = v.clamp(0.0, 1.0) as f32;
            }
        }
    }
    if s.soften_px > 0.0 {
        let kernel = effect_blur_kernel(s.soften_px)?;
        let soft = blur_ext(&out, g, &kernel, 0.0)?;
        return Ok(soft.iter().map(|v| v.clamp(0.0, 1.0)).collect());
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
pub(crate) fn gradient_sample(o: &GradientOverlayOp, t: f64) -> ([f32; 3], f32) {
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

// ---- pattern overlay (lane SMB-PS-ADJUST-STYLES-2; STU-RAS-157 `pattern_overlay`: pattern, scale,
// phase_x, phase_y, link_with_layer) ----------------------------------------------------------------

/// Largest side of a pattern tile (a Pigment ceiling; STU-RAS-157 declares none).
pub const MAX_PATTERN_SIDE: u32 = 4096;

/// The pattern a pattern overlay tiles: a plan tile source (`key`) of `width x height` straight
/// RGBA pixels, with the hashes of its colour and alpha planes (the backend verifies them like a
/// `TileInput`'s).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct PatternRef {
    pub key: crate::render_plan::SourceKey,
    pub width: u32,
    pub height: u32,
    pub colour_sha256: [u8; 32],
    pub alpha_sha256: [u8; 32],
}

/// Pattern overlay: `scale` is the STU-RAS-157 percent as a fraction (`[0.01, 10]`), `phase` the
/// pattern offset in pixels, `link_with_layer` anchors the pattern at the layer's top-left
/// (otherwise at the document origin). Composited over the content, inside the shape.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct PatternOverlayOp {
    pub mode: StudioBlendMode,
    pub opacity: f32,
    pub pattern: PatternRef,
    pub scale: f32,
    pub phase: [f32; 2],
    pub link_with_layer: bool,
}
impl PatternOverlayOp {
    /// Scale 1, no phase, linked with the layer.
    pub const fn new(mode: StudioBlendMode, opacity: f32, pattern: PatternRef) -> Self {
        Self {
            mode,
            opacity,
            pattern,
            scale: 1.0,
            phase: [0.0; 2],
            link_with_layer: true,
        }
    }
    pub const fn with_scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }
    pub const fn with_phase(mut self, phase: [f32; 2]) -> Self {
        self.phase = phase;
        self
    }
    pub const fn with_link(mut self, link_with_layer: bool) -> Self {
        self.link_with_layer = link_with_layer;
        self
    }
    /// The pattern pixel (straight RGBA) covering the centre of pixel `(x, y)` for the layer
    /// bounds `layer`: the pattern coordinate is `floor((x + 0.5 - origin - phase) / scale)`
    /// wrapped into the pattern (nearest pixel; Studio reading, Photoshop's resampling UNVERIFIED
    /// D5). `None` when `pixels` is not `width x height` long.
    pub fn sample_at(&self, x: i64, y: i64, layer: crate::Rect, pixels: &[[f32; 4]]) -> Option<[f32; 4]> {
        let (ox, oy) = if self.link_with_layer {
            (layer.x as f64, layer.y as f64)
        } else {
            (0.0, 0.0)
        };
        let s = f64::from(self.scale);
        let (w, h) = (f64::from(self.pattern.width), f64::from(self.pattern.height));
        let u = ((x as f64 + 0.5 - ox - f64::from(self.phase[0])) / s).floor().rem_euclid(w);
        let v = ((y as f64 + 0.5 - oy - f64::from(self.phase[1])) / s).floor().rem_euclid(h);
        if !(u.is_finite() && v.is_finite()) {
            return None;
        }
        // u, v are integers in [0, side) with side <= MAX_PATTERN_SIDE
        let (u, v) = (u as usize, v as usize);
        let width = usize::try_from(self.pattern.width).ok()?;
        let len = width.checked_mul(usize::try_from(self.pattern.height).ok()?)?;
        if pixels.len() != len {
            return None;
        }
        pixels.get(v.checked_mul(width)?.checked_add(u)?).copied()
    }
}

/// Pattern pixels by plan source key: straight RGBA, row-major, the pattern's `width x height`.
pub trait PatternPixels {
    fn pattern(&self, key: crate::render_plan::SourceKey) -> Option<&[[f32; 4]]>;
}

/// No pattern pixels (a pattern overlay is then `Unsupported(PatternUnresolved)`).
pub struct NoPatterns;
impl PatternPixels for NoPatterns {
    fn pattern(&self, _: crate::render_plan::SourceKey) -> Option<&[[f32; 4]]> {
        None
    }
}

pub(super) fn validate_pattern(o: &PatternOverlayOp) -> Result<(), EffectError> {
    if !o.scale.is_finite()
        || !(0.01..=10.0).contains(&o.scale)
        || !o.phase.iter().all(|v| v.is_finite() && v.abs() <= 1.0e6)
    {
        return Err(EffectError::OutOfDomain);
    }
    let side = |v: u32| (1..=MAX_PATTERN_SIDE).contains(&v);
    if !side(o.pattern.width) || !side(o.pattern.height) {
        return Err(EffectError::SizeOutOfRange);
    }
    Ok(())
}

/// The pixels of `o`'s pattern, checked against its size (typed: unresolved, wrong length).
pub(super) fn pattern_pixels<'p>(
    o: &PatternOverlayOp,
    patterns: &'p dyn PatternPixels,
) -> Result<&'p [[f32; 4]], EffectError> {
    let pixels = patterns
        .pattern(o.pattern.key)
        .ok_or(EffectError::Unsupported(EffectFeature::PatternUnresolved))?;
    let len = usize::try_from(o.pattern.width)
        .ok()
        .zip(usize::try_from(o.pattern.height).ok())
        .and_then(|(w, h)| w.checked_mul(h))
        .ok_or(EffectError::Overflow)?;
    if pixels.len() != len || !pixels.iter().all(|p| p.iter().all(|c| unit(*c))) {
        return Err(EffectError::PlaneShape);
    }
    Ok(pixels)
}
