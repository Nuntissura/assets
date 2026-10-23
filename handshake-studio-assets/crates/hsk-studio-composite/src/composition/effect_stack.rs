//! Per-layer effect stack of a composition layer (lane SMB-AE-EFFECT-STACK-1; parity AE-17):
//! [`EffectedContent`] and the CPU kernels of the distort and generate families it runs. Folio-free
//! and motion-free like every scene content; `temporal::composition` lowers the Folio
//! `StudioCompositionLayer::effects` records into it.
//!
//! Spec basis (module 14, v02.221): [STU-CMP-010] "For each layer, in order: 1. Source resolution
//! ... 2. Masks ... multiply it into the layer's alpha. 3. Effects. Evaluate the
//! `StudioEffectStack` in stack order ([STU-FX-002]). Effects therefore see MASKED content ... 5.
//! Transform." and [STU-CMP-010b] "masks are applied BEFORE effects, and the transform is applied
//! AFTER effects": the inner content is rendered over [`EffectedContent::rect`] (layer space, no
//! transform), multiplied by the layer-space mask coverage when the adapter folded the layer's
//! masks in, every entry runs in list order, and the result is placed like a tile with the layer
//! matrix. [STU-FX-003] "Every stack entry is re-editable, reorderable ... individually
//! enable/disable-able ... Reordering two entries re-composes the stack in the new order
//! deterministically": the stack is evaluated from the preserved source on every render.
//! [STU-FX-008] "Each stack entry composes onto the accumulated result using its `StudioBlendMode` +
//! opacity": [`SceneEffectEntry::mix`] mixes the entry's result with the pre-entry result (normal
//! mode only; other entry blend modes are not modelled). [STU-FX-011] "Effect evaluation is
//! deterministic": scalar `f64` maths in a fixed order, no threads, no fused multiply-add; the
//! trigonometric kernels use the platform `libm`, so byte stability is per platform and backend
//! (the pinned digests are taken on the build platform).
//!
//! Pixel model: the working buffer is premultiplied RGBA `f32` over the rect, row-major; pixel
//! `(col, row)` has its centre at `(col + 0.5, row + 0.5)`; every coordinate of [`SceneEffect`] is
//! in these surface pixels. Distorts sample the pre-entry buffer bilinearly at texel centres,
//! transparent outside the rect (`Offset` wraps). The result is converted back to the straight
//! surface the scene path places (alpha 0 -> colour 0).
//!
//! Kernel readings (the parameter semantics are Studio readings; After Effects' exact formulas
//! are UNVERIFIED, D5 oracle):
//! - `Twirl`: inside `radius_px` of `centre` the source is rotated by
//!   `angle * (1 - r / radius_px)` (linear falloff, positive = clockwise on screen); outside,
//!   identity.
//! - `Spherize`: inside `radius_px`, the normalised radius `d` samples the source at
//!   `asin(d) * 2 / pi` (a hemisphere seen from above: the centre is magnified, the rim is
//!   continuous); outside, identity.
//! - `Offset`: the content moves by `shift` with wrap-around; `blend` mixes the original back in.
//! - `PolarCoordinates`: the conversion is centred on the rect; the polar radius spans half the
//!   smaller side; angle 0 points up and grows clockwise; `interpolation` moves the sample point
//!   linearly from identity (0) to the full conversion (1).
//! - `GradientRamp`: linear = projection onto `start -> end` clamped to `0..1`; radial = distance
//!   from `start` over `|end - start|` clamped; a degenerate ramp (`start == end`) is the end colour
//!   (radial) or the start colour (linear); colours interpolate in straight RGBA; the ramp replaces
//!   the layer except for `blend` of the original.
//! - `Circle`: a disc of `radius_px` around `centre`, edge antialiased over one pixel
//!   (`clamp(r + 0.5 - d)`), or feathered outward linearly over `feather_px`; radius `<= 0` draws
//!   nothing.
//! - `Checkerboard`: cells of `cell` pixels anchored at `anchor`; the cell to the lower right of the
//!   anchor is coloured, cells alternate.
//! - Generators composite by [`GeneratorComposite`]: `Replace` (option `None`: only the generated
//!   pixels remain) or `Over` (option `Normal`: generated over the layer, source-over).
//!
//! Hostile input (V4-01/02): every number is checked finite and in its domain before any buffer is
//! allocated (`EffectError::Parameter` names the entry), the entry count is at most
//! [`MAX_SCENE_EFFECTS`], the work `entries x pixels` at most [`MAX_EFFECT_WORK`], every surface is
//! charged before allocation, cancellation is checked per row, integer arithmetic is checked.
#![deny(clippy::arithmetic_side_effects)]
use super::{Charges, SceneContent, SceneContext, Surface, canceled, content_surface, f32_at, invalid};
use crate::CompositeError;
use crate::temporal::TemporalError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{Exactness, ExactnessLedger};
use hsk_studio_pigment::transform_math::ResampleFilter;
use std::f64::consts::{FRAC_2_PI, TAU};

/// Exactness-ledger key of the effect stack (bilinear resampling; the parameter semantics are
/// Studio readings, so the class is `Approximate`).
pub const EFFECT_STACK_OPERATOR: &str = "composition_effect_stack";
/// Most entries one [`EffectedContent`] runs (Folio `MAX_LAYER_EFFECTS`).
pub const MAX_SCENE_EFFECTS: usize = 128;
/// Most `entries x pixels` one stack evaluates.
pub const MAX_EFFECT_WORK: u64 = 1 << 34;

/// How a generator's pixels combine with the layer (the generators' `Blending Mode` options 1 and
/// 2; every other option is refused by the adapter).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneratorComposite {
    /// Option `None`: the generated pixels replace the layer.
    Replace,
    /// Option `Normal`: the generated pixels composite over the layer (source-over).
    Over,
}

/// One evaluated effect at a tick, coordinates in surface pixels (module docs).
#[derive(Clone, Debug, PartialEq)]
pub enum SceneEffect {
    Twirl { angle_deg: f64, radius_px: f64, centre: [f64; 2] },
    Spherize { radius_px: f64, centre: [f64; 2] },
    Offset { shift: [f64; 2], blend: f64 },
    PolarCoordinates { interpolation: f64, rect_to_polar: bool },
    GradientRamp { start: [f64; 2], start_colour: [f64; 4], end: [f64; 2], end_colour: [f64; 4], radial: bool, blend: f64 },
    Circle { centre: [f64; 2], radius_px: f64, feather_px: f64, colour: [f64; 4], opacity: f64, composite: GeneratorComposite },
    Checkerboard { anchor: [f64; 2], cell: [f64; 2], colour: [f64; 4], opacity: f64, composite: GeneratorComposite },
}

/// One stack entry: the effect and the fraction `0..=1` of its result mixed over the pre-entry
/// result ([STU-FX-008] entry opacity).
#[derive(Clone, Debug, PartialEq)]
pub struct SceneEffectEntry {
    pub effect_id: String,
    pub effect: SceneEffect,
    pub mix: f64,
}

/// Content under an effect stack (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct EffectedContent {
    pub content: SceneContent,
    /// The content's plan rect the stack runs over (layer space, before the layer matrix).
    pub rect: Rect,
    /// Layer-space mask coverage over `rect` (row-major, `0..=1`), multiplied into the content's
    /// alpha before the first entry ([STU-CMP-010] step 2); `None` = no masks folded in.
    pub mask: Option<Vec<f32>>,
    /// Entries in stack order (disabled and un-soloed entries already dropped by the adapter).
    pub effects: Vec<SceneEffectEntry>,
}

/// Refusals of [`apply_effects`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectError {
    /// Entry `index` carries a non-finite or out-of-domain number.
    Parameter(usize),
    /// The buffer does not match `width x height`.
    Shape,
    /// Entry count or work above the bounds.
    OverBudget,
    Overflow,
    Canceled,
}

fn finite(v: &[f64]) -> bool {
    v.iter().all(|x| x.is_finite())
}
fn unit(v: f64) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}

/// Domain check of one entry (module docs).
fn entry_ok(e: &SceneEffectEntry) -> bool {
    unit(e.mix)
        && match &e.effect {
            SceneEffect::Twirl { angle_deg, radius_px, centre } => angle_deg.is_finite() && radius_px.is_finite() && *radius_px >= 0.0 && finite(centre),
            SceneEffect::Spherize { radius_px, centre } => radius_px.is_finite() && *radius_px >= 0.0 && finite(centre),
            SceneEffect::Offset { shift, blend } => finite(shift) && unit(*blend),
            SceneEffect::PolarCoordinates { interpolation, .. } => unit(*interpolation),
            SceneEffect::GradientRamp { start, start_colour, end, end_colour, blend, .. } => {
                finite(start) && finite(end) && finite(start_colour) && finite(end_colour) && unit(*blend)
            }
            SceneEffect::Circle { centre, radius_px, feather_px, colour, opacity, .. } => {
                finite(centre) && radius_px.is_finite() && feather_px.is_finite() && *feather_px >= 0.0 && finite(colour) && unit(*opacity)
            }
            SceneEffect::Checkerboard { anchor, cell, colour, opacity, .. } => {
                finite(anchor) && finite(cell) && cell.iter().all(|c| *c > 0.0) && finite(colour) && unit(*opacity)
            }
        }
}

/// The texel `(x, y)` of `src`, transparent outside (`wrap` = wrap-around addressing).
fn texel(src: &[[f32; 4]], w: u32, h: u32, x: f64, y: f64, wrap: bool) -> [f64; 4] {
    let (fw, fh) = (f64::from(w), f64::from(h));
    let (x, y) = if wrap { (x.rem_euclid(fw), y.rem_euclid(fh)) } else { (x, y) };
    if !(x >= 0.0 && y >= 0.0 && x < fw && y < fh) {
        return [0.0; 4];
    }
    // In range: exact non-negative integers below u32::MAX.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let at = (y as usize).checked_mul(w as usize).and_then(|r| r.checked_add(x as usize));
    at.and_then(|i| src.get(i)).map_or([0.0; 4], |p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2]), f64::from(p[3])])
}

/// Bilinear sample of `src` at surface point `(u, v)` (texel centres at `+0.5`).
fn bilinear(src: &[[f32; 4]], w: u32, h: u32, u: f64, v: f64, wrap: bool) -> [f64; 4] {
    if !(u.is_finite() && v.is_finite()) || u.abs() > 1.0e9 || v.abs() > 1.0e9 {
        return [0.0; 4];
    }
    let (fx, fy) = (u - 0.5, v - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let a = texel(src, w, h, x0, y0, wrap);
    let b = texel(src, w, h, x0 + 1.0, y0, wrap);
    let c = texel(src, w, h, x0, y0 + 1.0, wrap);
    let d = texel(src, w, h, x0 + 1.0, y0 + 1.0, wrap);
    let mut out = [0.0; 4];
    for (k, o) in out.iter_mut().enumerate() {
        let top = a[k] + (b[k] - a[k]) * tx;
        let bottom = c[k] + (d[k] - c[k]) * tx;
        *o = top + (bottom - top) * ty;
    }
    out
}

fn premultiplied(c: [f64; 4], alpha_scale: f64) -> [f64; 4] {
    let a = c[3] * alpha_scale;
    [c[0] * a, c[1] * a, c[2] * a, a]
}
fn over(top: [f64; 4], bottom: [f64; 4]) -> [f64; 4] {
    let k = 1.0 - top[3];
    [top[0] + bottom[0] * k, top[1] + bottom[1] * k, top[2] + bottom[2] * k, top[3] + bottom[3] * k]
}
fn lerp4(a: [f64; 4], b: [f64; 4], t: f64) -> [f64; 4] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t]
}
fn generated(g: [f64; 4], pre: [f64; 4], composite: GeneratorComposite) -> [f64; 4] {
    match composite {
        GeneratorComposite::Replace => g,
        GeneratorComposite::Over => over(g, pre),
    }
}

/// The effect's premultiplied value at pixel centre `(u, v)` given the pre-entry buffer `src`.
fn evaluate(effect: &SceneEffect, src: &[[f32; 4]], w: u32, h: u32, u: f64, v: f64, pre: [f64; 4]) -> [f64; 4] {
    let (fw, fh) = (f64::from(w), f64::from(h));
    match effect {
        SceneEffect::Twirl { angle_deg, radius_px, centre } => {
            let (dx, dy) = (u - centre[0], v - centre[1]);
            let r = dx.hypot(dy);
            if *radius_px <= 0.0 || r >= *radius_px {
                return pre;
            }
            let theta = -angle_deg.to_radians() * (1.0 - r / radius_px);
            let (s, c) = theta.sin_cos();
            bilinear(src, w, h, centre[0] + dx * c - dy * s, centre[1] + dx * s + dy * c, false)
        }
        SceneEffect::Spherize { radius_px, centre } => {
            let (dx, dy) = (u - centre[0], v - centre[1]);
            let r = dx.hypot(dy);
            if *radius_px <= 0.0 || r >= *radius_px || r <= 0.0 {
                return pre;
            }
            let d = r / radius_px;
            let k = d.asin() * FRAC_2_PI / d;
            bilinear(src, w, h, centre[0] + dx * k, centre[1] + dy * k, false)
        }
        SceneEffect::Offset { shift, blend } => {
            let moved = bilinear(src, w, h, u - shift[0], v - shift[1], true);
            lerp4(moved, pre, *blend)
        }
        SceneEffect::PolarCoordinates { interpolation, rect_to_polar } => {
            let (cx, cy) = (fw / 2.0, fh / 2.0);
            let rmax = fw.min(fh) / 2.0;
            let (su, sv) = if *rect_to_polar {
                let (dx, dy) = (u - cx, v - cy);
                let a = dx.atan2(-dy).rem_euclid(TAU);
                (a / TAU * fw, dx.hypot(dy) / rmax * fh)
            } else {
                let a = u / fw * TAU;
                let r = v / fh * rmax;
                let (s, c) = a.sin_cos();
                (cx + r * s, cy - r * c)
            };
            bilinear(src, w, h, u + (su - u) * interpolation, v + (sv - v) * interpolation, false)
        }
        SceneEffect::GradientRamp { start, start_colour, end, end_colour, radial, blend } => {
            let (ex, ey) = (end[0] - start[0], end[1] - start[1]);
            let (px, py) = (u - start[0], v - start[1]);
            let t = if *radial {
                let len = ex.hypot(ey);
                if len > 0.0 { px.hypot(py) / len } else { 1.0 }
            } else {
                let l2 = ex * ex + ey * ey;
                if l2 > 0.0 { (px * ex + py * ey) / l2 } else { 0.0 }
            };
            let ramp = premultiplied(lerp4(*start_colour, *end_colour, t.clamp(0.0, 1.0)), 1.0);
            lerp4(ramp, pre, *blend)
        }
        SceneEffect::Circle { centre, radius_px, feather_px, colour, opacity, composite } => {
            let d = (u - centre[0]).hypot(v - centre[1]);
            let cover = if *radius_px <= 0.0 {
                0.0
            } else if *feather_px > 0.0 {
                ((radius_px + feather_px - d) / feather_px).clamp(0.0, 1.0)
            } else {
                (radius_px + 0.5 - d).clamp(0.0, 1.0)
            };
            generated(premultiplied(*colour, cover * opacity), pre, *composite)
        }
        SceneEffect::Checkerboard { anchor, cell, colour, opacity, composite } => {
            let i = ((u - anchor[0]) / cell[0]).floor();
            let j = ((v - anchor[1]) / cell[1]).floor();
            let coloured = (i + j).rem_euclid(2.0) < 0.5;
            let g = if coloured { premultiplied(*colour, *opacity) } else { [0.0; 4] };
            generated(g, pre, *composite)
        }
    }
}

/// Runs `effects` in order over the premultiplied buffer `pixels` (`width x height`, row-major),
/// in place (module docs). Every entry is checked before the first runs.
pub fn apply_effects(
    width: u32,
    height: u32,
    pixels: &mut [[f32; 4]],
    effects: &[SceneEffectEntry],
    cancel: &CancellationToken,
) -> Result<(), EffectError> {
    let n = u64::from(width).checked_mul(u64::from(height)).ok_or(EffectError::Overflow)?;
    if u64::try_from(pixels.len()).map_err(|_| EffectError::Overflow)? != n {
        return Err(EffectError::Shape);
    }
    if effects.len() > MAX_SCENE_EFFECTS {
        return Err(EffectError::OverBudget);
    }
    let work = u64::try_from(effects.len()).map_err(|_| EffectError::Overflow)?.checked_mul(n).ok_or(EffectError::Overflow)?;
    if work > MAX_EFFECT_WORK {
        return Err(EffectError::OverBudget);
    }
    if let Some(bad) = effects.iter().position(|e| !entry_ok(e)) {
        return Err(EffectError::Parameter(bad));
    }
    if effects.is_empty() {
        return Ok(());
    }
    let w = usize::try_from(width).map_err(|_| EffectError::Overflow)?;
    let mut src: Vec<[f32; 4]> = pixels.to_vec();
    for e in effects {
        src.copy_from_slice(pixels);
        for (row, line) in pixels.chunks_mut(w.max(1)).enumerate() {
            cancel.check().map_err(|_| EffectError::Canceled)?;
            #[allow(clippy::cast_precision_loss)]
            let v = row as f64 + 0.5;
            for (col, out) in line.iter_mut().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let u = col as f64 + 0.5;
                let pre = [f64::from(out[0]), f64::from(out[1]), f64::from(out[2]), f64::from(out[3])];
                let value = lerp4(pre, evaluate(&e.effect, &src, width, height, u, v, pre), e.mix);
                #[allow(clippy::cast_possible_truncation)]
                {
                    *out = [value[0] as f32, value[1] as f32, value[2] as f32, value[3] as f32];
                }
            }
        }
    }
    Ok(())
}

fn effect_err(id: &str, e: EffectError) -> TemporalError {
    match e {
        EffectError::Parameter(_) => invalid(id, "effect_parameter"),
        EffectError::Shape => invalid(id, "effect_rect"),
        EffectError::OverBudget => CompositeError::BudgetExceeded.into(),
        EffectError::Overflow => CompositeError::Overflow.into(),
        EffectError::Canceled => canceled(),
    }
}

/// [`super::SceneContent::Effected`] (module docs).
#[allow(clippy::too_many_arguments)]
pub(super) fn effected_surface(
    id: &str,
    e: &EffectedContent,
    filter: ResampleFilter,
    ctx: &SceneContext<'_>,
    cancel: &CancellationToken,
    depth: usize,
    charges: &mut Charges,
    exactness: &mut ExactnessLedger,
) -> Result<Surface, TemporalError> {
    let n = usize::try_from(e.rect.area()).map_err(|_| CompositeError::Overflow)?;
    if e.rect.width == 0 || e.rect.height == 0 || e.mask.as_ref().is_some_and(|m| m.len() != n || m.iter().any(|c| !(0.0..=1.0).contains(c))) {
        return Err(invalid(id, "effect_rect"));
    }
    // Bounds and parameters are refused before the content renders or a buffer is allocated.
    let work = u64::try_from(e.effects.len()).map_err(|_| CompositeError::Overflow)?.checked_mul(e.rect.area()).ok_or(CompositeError::Overflow)?;
    if e.effects.len() > MAX_SCENE_EFFECTS || work > MAX_EFFECT_WORK {
        return Err(CompositeError::BudgetExceeded.into());
    }
    if e.effects.iter().any(|x| !entry_ok(x)) {
        return Err(invalid(id, "effect_parameter"));
    }
    let mut surface = content_surface(id, &e.content, e.rect, filter, ctx, cancel, depth, charges, exactness)?;
    if surface.width != e.rect.width || surface.height != e.rect.height {
        return Err(invalid(id, "effect_rect"));
    }
    // The working buffer and the pre-entry copy (16 bytes per pixel each).
    charges.surface(ctx, e.rect.width, e.rect.height)?;
    charges.surface(ctx, e.rect.width, e.rect.height)?;
    let mut buf: Vec<[f32; 4]> = Vec::with_capacity(n);
    for i in 0..n {
        let base = i.checked_mul(3).ok_or(CompositeError::Overflow)?;
        let a = f64::from(f32_at(&surface.alpha, i)) * e.mask.as_ref().and_then(|m| m.get(i)).map_or(1.0, |c| f64::from(*c));
        let rgb = [0usize, 1, 2].map(|k| base.checked_add(k).map_or(0.0, |o| f64::from(f32_at(&surface.colour, o))));
        #[allow(clippy::cast_possible_truncation)]
        buf.push([(rgb[0] * a) as f32, (rgb[1] * a) as f32, (rgb[2] * a) as f32, a as f32]);
    }
    apply_effects(e.rect.width, e.rect.height, &mut buf, &e.effects, cancel).map_err(|x| effect_err(id, x))?;
    for (i, p) in buf.iter().enumerate() {
        let a = p[3];
        let straight = if a > 0.0 { [p[0] / a, p[1] / a, p[2] / a] } else { [0.0; 3] };
        let base = i.checked_mul(12).ok_or(CompositeError::Overflow)?;
        for (k, v) in straight.iter().enumerate() {
            let o = base.checked_add(k.checked_mul(4).ok_or(CompositeError::Overflow)?).ok_or(CompositeError::Overflow)?;
            if let Some(slot) = surface.colour.get_mut(o..o.checked_add(4).ok_or(CompositeError::Overflow)?) {
                slot.copy_from_slice(&v.to_le_bytes());
            }
        }
        let o = i.checked_mul(4).ok_or(CompositeError::Overflow)?;
        if let Some(slot) = surface.alpha.get_mut(o..o.checked_add(4).ok_or(CompositeError::Overflow)?) {
            slot.copy_from_slice(&a.max(0.0).to_le_bytes());
        }
    }
    surface.colour_sha256 = hsk_studio_prism::profile_hash(&surface.colour);
    surface.alpha_sha256 = hsk_studio_prism::profile_hash(&surface.alpha);
    exactness.record(EFFECT_STACK_OPERATOR, Exactness::Approximate);
    Ok(surface)
}
