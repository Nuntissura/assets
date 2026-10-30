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
//! Blur, glow and colour families and entry semantics (lane SMB-AE-EFFECT-FAMILIES-2, parity
//! AE-17; Studio readings, After Effects' maths UNVERIFIED, D5). These kernels run over the whole
//! pre-entry buffer and REUSE pigment's single owners (CX-CAP-001), never a copy:
//! - `Blur`: pigment `blur_math::blur_window` (premultiplied separable convolution, both axes) or
//!   `blur_window_axis` (one axis: Blur Dimensions Horizontal / Vertical) on the straight pre-entry
//!   buffer (`blur_math::unpremultiply`) with the entry's taps: Gaussian Blur takes pigment's
//!   Gaussian (`LayerBlur::taps`, `sigma = radius / 2`); Fast Box Blur pigment's iterated box
//!   (`box_kernel`). `repeat_edges` extends the source by its border texels over the kernel support
//!   (Repeat Edge Pixels), else the source is transparent beyond the rect. The output is clipped to
//!   the content rect (the layer does not grow). A single tap `[1.0]` is the identity: the
//!   pre-entry buffer is kept bit for bit.
//! - `DirectionalBlur` / `RadialBlur` / `FindEdges`: pigment's `directional_blur_window`,
//!   `radial_blur_window` (Spin | Zoom) and `filter_math::find_edges_window` (lane
//!   SMB-PIGMENT-AE-KERNELS-1) on the straight pre-entry buffer, transparent beyond the rect; Find
//!   Edges mixes the original back in by `blend` (premultiplied). Length 0, amount 0 and blend 1 are
//!   the identity bit for bit.
//! - `Glow`: bright pass by pigment's `Threshold` adjustment on the straight colour (or on alpha,
//!   `based_on_alpha`); threshold `0` (or below one 8-bit level) passes every texel and `1` (100 %)
//!   none; the passed texels are blurred by `blur_window`, scaled by `intensity` in premultiplied
//!   form (alpha at most 1), then keep their own colours or take the A & B ramp ([`GlowColours`]);
//!   [`GlowOriginal::Behind`] composites the glow over the original with `mode` (pigment
//!   `blend_math::composite_over`), [`GlowOriginal::OnTop`] the original over the glow (normal),
//!   [`GlowOriginal::None`] keeps the glow alone. A texel without glow keeps the pre-entry value
//!   bit for bit (`None`: transparent).
//! - `Colour`: pigment `adjust_math` adjustments on the straight colour (alpha unchanged, transparent
//!   texels unchanged); every [`ChannelAdjustment`] reads the entry's input colour and writes only
//!   its channels; `blend` mixes the input back in (`out (1 - blend) + input blend`).
//! - `Composite` ([STU-FX-004], [STU-FX-008]): the entry's own blend mode and effect mask wrap the
//!   inner effect. A mode other than normal replaces the result's colour by the W3C blended source
//!   colour `Cs' = (1 - ab) Cs + ab B(Cb, Cs)` against the pre-entry colour (pigment
//!   `blend_math::blend_rgb`, the layer blend code; Compositing 1 s5.1), alpha kept; the mask
//!   coverage multiplies the entry mix per texel. Normal without a mask is never wrapped, so the
//!   stacks of SMB-AE-EFFECT-STACK-1 keep their bytes.
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
use hsk_studio_pigment::adjust_math::{AdjustmentOp, PreparedAdjustment};
use hsk_studio_pigment::blend_math::{Exactness, ExactnessLedger, PixelSite, Rgba, StudioBlendMode, blend_rgb, composite_over};
use hsk_studio_pigment::blur_math::{
    BlurAxis, BlurError, BlurLimits, MAX_BOX_KERNEL_TAPS, MAX_DIRECTIONAL_BLUR_LENGTH_PX, MAX_RADIAL_BLUR_AMOUNT, RadialBlurKind,
    blur_window, blur_window_axis, directional_blur_window, radial_blur_window, unpremultiply,
};
use hsk_studio_pigment::filter_math::{FilterError, FilterLimits, find_edges_window};
use hsk_studio_pigment::render_plan::SampleRange;
use hsk_studio_pigment::transform_math::ResampleFilter;
use std::f64::consts::{FRAC_2_PI, TAU};

/// Exactness-ledger key of the effect stack (bilinear resampling; the parameter semantics are
/// Studio readings, so the class is `Approximate`).
pub const EFFECT_STACK_OPERATOR: &str = "composition_effect_stack";
/// Most entries one [`EffectedContent`] runs (Folio `MAX_LAYER_EFFECTS`).
pub const MAX_SCENE_EFFECTS: usize = 128;
/// Most `entries x pixels` one stack evaluates.
pub const MAX_EFFECT_WORK: u64 = 1 << 34;
/// Most taps of one blur kernel (pigment's `MAX_BOX_KERNEL_TAPS`; its Gaussian at the largest
/// radius has 3001).
pub const MAX_EFFECT_TAPS: usize = MAX_BOX_KERNEL_TAPS;
/// Most bytes one blur of the stack holds (the padded source, pigment's window and its output).
pub const MAX_EFFECT_BLUR_BYTES: u64 = 1 << 30;

/// Where a glow's original goes (Glow `Composite Original`, Studio reading of the option names).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlowOriginal {
    /// Option `On Top`: the original composites (normal) over the glow.
    OnTop,
    /// Option `Behind`: the glow composites over the original with the glow operation.
    Behind,
    /// Option `None`: the glow alone.
    None,
}

/// Glow `Color Looping` options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlowLooping {
    SawtoothAb,
    SawtoothBa,
    TriangleAba,
    TriangleBab,
}

/// Glow `A & B Colors` (Studio reading): the glow's alpha `p` (after intensity) is the ramp input;
/// `x = p loops`, `f = fract(x)` (`1` at a whole positive `x`); sawtooth A>B takes `f`, B>A `1 - f`,
/// triangle A>B>A `1 - |2f - 1|`, B>A>B `|2f - 1|`; the A & B midpoint `m` maps `t` piecewise
/// linearly (`t < m`: `t / 2m`, else `1/2 + (t - m) / 2(1 - m)`); the colour is the straight lerp
/// of `a` to `b`, its alpha multiplies the glow alpha.
#[derive(Clone, Debug, PartialEq)]
pub struct GlowColours {
    pub a: [f64; 4],
    pub b: [f64; 4],
    pub looping: GlowLooping,
    pub loops: f64,
    pub midpoint: f64,
}
impl GlowColours {
    /// Ramp position `0..=1` of glow alpha `p` (type docs).
    #[allow(clippy::float_cmp)]
    pub fn position(&self, p: f64) -> f64 {
        let x = p * self.loops;
        let mut f = x - x.floor();
        if f == 0.0 && x > 0.0 {
            f = 1.0;
        }
        let t = match self.looping {
            GlowLooping::SawtoothAb => f,
            GlowLooping::SawtoothBa => 1.0 - f,
            GlowLooping::TriangleAba => 1.0 - (2.0 * f - 1.0).abs(),
            GlowLooping::TriangleBab => (2.0 * f - 1.0).abs(),
        };
        let m = self.midpoint;
        if t < m { 0.5 * t / m } else { 0.5 + 0.5 * (t - m) / (1.0 - m) }
    }
}

/// Glow (module docs). `threshold` is a fraction `0..=1`; `taps` the blur kernel.
#[derive(Clone, Debug, PartialEq)]
pub struct GlowEffect {
    pub based_on_alpha: bool,
    pub threshold: f64,
    pub taps: Vec<f32>,
    /// Glow Dimensions: `None` = both axes, else the one axis.
    pub axis: Option<BlurAxis>,
    pub intensity: f64,
    pub original: GlowOriginal,
    /// The glow operation (used by [`GlowOriginal::Behind`]).
    pub mode: StudioBlendMode,
    /// `None` = the glow keeps its own (original) colours.
    pub colours: Option<GlowColours>,
}

/// One pigment adjustment and the output channels `[r, g, b]` it writes.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelAdjustment {
    pub op: AdjustmentOp,
    pub channels: [bool; 3],
}

/// Colour family entry (module docs): `blend` (`0..=1`) mixes the input colour back in.
#[derive(Clone, Debug, PartialEq)]
pub struct ColourEffect {
    pub ops: Vec<ChannelAdjustment>,
    pub blend: f64,
}

/// An entry's own composite options around its effect (module docs): [STU-FX-008] blend mode and
/// [STU-FX-004] effect-mask coverage over the rect (row-major, `0..=1`).
#[derive(Clone, Debug, PartialEq)]
pub struct EntryComposite {
    pub effect: SceneEffect,
    pub mode: StudioBlendMode,
    pub mask: Option<Vec<f32>>,
}


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
    /// Gaussian Blur / Fast Box Blur (module docs).
    /// `axis`: `None` = both axes (Blur Dimensions `Horizontal and Vertical`), else the one axis.
    Blur { taps: Vec<f32>, axis: Option<BlurAxis>, repeat_edges: bool },
    /// Directional Blur: pigment's `directional_blur_window` angle (from +x towards screen-up) and
    /// segment length in surface pixels.
    DirectionalBlur { angle_deg: f64, length_px: f64 },
    /// Radial Blur around `centre` (surface pixels) by pigment's `radial_blur_window`.
    RadialBlur { centre: [f64; 2], amount: f64, kind: RadialBlurKind },
    /// Find Edges (pigment `find_edges_window`); `blend` (`0..=1`) mixes the original back in.
    FindEdges { invert: bool, blend: f64 },
    Glow(Box<GlowEffect>),
    Colour(Box<ColourEffect>),
    /// An entry's blend mode and effect mask around its effect (module docs).
    Composite(Box<EntryComposite>),
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

fn taps_ok(taps: &[f32]) -> bool {
    !taps.len().is_multiple_of(2)
        && taps.len() <= MAX_EFFECT_TAPS
        && taps.iter().all(|t| t.is_finite() && *t >= 0.0)
        && taps.iter().sum::<f32>() > 0.0
}
/// A blend mode an entry or a glow can use: one pigment's `blend_rgb` defines (dissolve acts on
/// alpha only, so it is not one).
fn mode_ok(mode: StudioBlendMode) -> bool {
    mode != StudioBlendMode::Dissolve && blend_rgb(mode, [0.5; 3], [0.5; 3]).is_ok()
}
fn identity_taps(taps: &[f32]) -> bool {
    taps == [1.0]
}

/// Domain check of one entry (module docs).
fn entry_ok(e: &SceneEffectEntry) -> bool {
    unit(e.mix) && effect_ok(&e.effect)
}
/// The entry's effect mask matches the `n` texels of the rect.
fn mask_ok(e: &SceneEffectEntry, n: usize) -> bool {
    match &e.effect {
        SceneEffect::Composite(c) => c.mask.as_ref().is_none_or(|m| m.len() == n),
        _ => true,
    }
}
fn effect_ok(effect: &SceneEffect) -> bool {
    match effect {
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
            SceneEffect::Blur { taps, .. } => taps_ok(taps),
            SceneEffect::DirectionalBlur { angle_deg, length_px } => {
                angle_deg.is_finite() && length_px.is_finite() && (0.0..=f64::from(MAX_DIRECTIONAL_BLUR_LENGTH_PX)).contains(length_px)
            }
            SceneEffect::RadialBlur { centre, amount, .. } => {
                finite(centre) && centre.iter().all(|c| c.abs() <= 1.0e9) && amount.is_finite() && (0.0..=f64::from(MAX_RADIAL_BLUR_AMOUNT)).contains(amount)
            }
            SceneEffect::FindEdges { blend, .. } => unit(*blend),
            SceneEffect::Glow(g) => {
                taps_ok(&g.taps)
                    && unit(g.threshold)
                    && g.intensity.is_finite()
                    && g.intensity >= 0.0
                    && mode_ok(g.mode)
                    && g.colours.as_ref().is_none_or(|c| {
                        finite(&c.a) && finite(&c.b) && c.loops.is_finite() && c.loops >= 1.0 && c.midpoint > 0.0 && c.midpoint < 1.0
                    })
            }
            SceneEffect::Colour(c) => unit(c.blend) && c.ops.iter().all(|o| o.op.prepare().is_ok()),
            SceneEffect::Composite(c) => {
                !matches!(c.effect, SceneEffect::Composite(_))
                    && effect_ok(&c.effect)
                    && mode_ok(c.mode)
                    && c.mask.as_ref().is_none_or(|m| m.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)))
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
        // Whole-buffer kernels ([`whole_buffer`]) never reach the per-texel path.
        SceneEffect::Blur { .. }
        | SceneEffect::DirectionalBlur { .. }
        | SceneEffect::RadialBlur { .. }
        | SceneEffect::FindEdges { .. }
        | SceneEffect::Glow(_)
        | SceneEffect::Colour(_)
        | SceneEffect::Composite(_) => pre,
    }
}

fn straight_of(src: &[[f32; 4]]) -> Vec<[f32; 4]> {
    src.iter().map(|p| unpremultiply(*p)).collect()
}
fn widen(p: [f32; 4]) -> [f64; 4] {
    [f64::from(p[0]), f64::from(p[1]), f64::from(p[2]), f64::from(p[3])]
}
/// A pigment refusal inside entry `index` is that entry's parameter refusal.
fn param<E>(index: usize) -> impl Fn(E) -> EffectError {
    move |_| EffectError::Parameter(index)
}
/// Premultiplied `f64` texel of a straight `f32` texel.
fn premultiply(s: [f32; 4]) -> [f64; 4] {
    let a = f64::from(s[3]);
    [f64::from(s[0]) * a, f64::from(s[1]) * a, f64::from(s[2]) * a, a]
}
#[allow(clippy::cast_possible_truncation)]
fn narrow(p: [f64; 4]) -> [f32; 4] {
    [p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32]
}

/// pigment's `blur_window` over the straight plane `straight` (`w x h`) with `taps`, border texels
/// repeated over the support when `repeat` (module docs). Every buffer is bounded before allocation.
#[allow(clippy::too_many_arguments)]
fn blur_plane(
    straight: &[[f32; 4]],
    w: u32,
    h: u32,
    taps: &[f32],
    axis: Option<BlurAxis>,
    repeat: bool,
    index: usize,
) -> Result<Vec<[f32; 4]>, EffectError> {
    let out = Rect { x: 0, y: 0, width: w, height: h };
    let limits = BlurLimits { max_bytes: MAX_EFFECT_BLUR_BYTES, max_work: MAX_EFFECT_WORK };
    let mapped = |e: BlurError| blur_err(e, index);
    let run = |plane: &[[f32; 4]], rect: Rect| match axis {
        None => blur_window(plane, rect, out, taps, limits).map_err(mapped),
        Some(a) => blur_window_axis(SampleRange::Unit, plane, rect, out, taps, a, limits).map_err(mapped),
    };
    let pad = u32::try_from(taps.len().checked_div(2).ok_or(EffectError::Overflow)?).map_err(|_| EffectError::Overflow)?;
    if !repeat || pad == 0 || w == 0 || h == 0 {
        return run(straight, out);
    }
    let twice = pad.checked_mul(2).ok_or(EffectError::Overflow)?;
    let pw = w.checked_add(twice).ok_or(EffectError::Overflow)?;
    let ph = h.checked_add(twice).ok_or(EffectError::Overflow)?;
    let area = u64::from(pw).checked_mul(u64::from(ph)).ok_or(EffectError::Overflow)?;
    if area.checked_mul(16).ok_or(EffectError::Overflow)? > MAX_EFFECT_BLUR_BYTES {
        return Err(EffectError::OverBudget);
    }
    let mut plane: Vec<[f32; 4]> = Vec::new();
    plane.try_reserve_exact(usize::try_from(area).map_err(|_| EffectError::Overflow)?).map_err(|_| EffectError::OverBudget)?;
    let pad64 = i64::from(pad);
    let last_x = i64::from(w).checked_sub(1).ok_or(EffectError::Overflow)?;
    let last_y = i64::from(h).checked_sub(1).ok_or(EffectError::Overflow)?;
    let wu = usize::try_from(w).map_err(|_| EffectError::Overflow)?;
    for py in 0..i64::from(ph) {
        let sy = py.checked_sub(pad64).ok_or(EffectError::Overflow)?.clamp(0, last_y);
        let row = usize::try_from(sy).ok().and_then(|r| r.checked_mul(wu)).ok_or(EffectError::Overflow)?;
        for px in 0..i64::from(pw) {
            let sx = px.checked_sub(pad64).ok_or(EffectError::Overflow)?.clamp(0, last_x);
            let at = usize::try_from(sx).ok().and_then(|c| row.checked_add(c)).ok_or(EffectError::Overflow)?;
            plane.push(*straight.get(at).ok_or(EffectError::Shape)?);
        }
    }
    let origin = 0_i64.checked_sub(pad64).ok_or(EffectError::Overflow)?;
    run(&plane, Rect { x: origin, y: origin, width: pw, height: ph })
}

fn blur_err(e: BlurError, index: usize) -> EffectError {
    match e {
        BlurError::OverBudget { .. } => EffectError::OverBudget,
        BlurError::Overflow => EffectError::Overflow,
        _ => EffectError::Parameter(index),
    }
}
fn filter_err(e: FilterError, index: usize) -> EffectError {
    match e {
        FilterError::OverBudget { .. } => EffectError::OverBudget,
        FilterError::Overflow => EffectError::Overflow,
        _ => EffectError::Parameter(index),
    }
}

/// The glow of `src` (premultiplied, `w x h`) as premultiplied texels (module docs).
fn glow(g: &GlowEffect, index: usize, src: &[[f32; 4]], w: u32, h: u32, cancel: &CancellationToken) -> Result<Vec<[f64; 4]>, EffectError> {
    let straight: Vec<[f32; 4]> = src.iter().map(|p| unpremultiply(*p)).collect();
    // Threshold 0 (or below one 8-bit level, pigment's smallest) passes every texel, 1 none.
    #[allow(clippy::cast_possible_truncation)]
    let level = g.threshold as f32;
    let threshold = if g.threshold >= 1.0 / 255.0 && g.threshold < 1.0 {
        Some(AdjustmentOp::Threshold { level }.prepare().map_err(param(index))?)
    } else {
        None
    };
    let mut bright = Vec::with_capacity(straight.len());
    for s in &straight {
        let pass = if g.threshold >= 1.0 {
            0.0
        } else if let Some(t) = &threshold {
            let probe = if g.based_on_alpha { [s[3]; 3] } else { [s[0], s[1], s[2]] };
            t.apply(probe).map_err(param(index))?[0]
        } else {
            1.0
        };
        bright.push([s[0], s[1], s[2], s[3] * pass]);
    }
    cancel.check().map_err(|_| EffectError::Canceled)?;
    let blurred = if identity_taps(&g.taps) { bright } else { blur_plane(&bright, w, h, &g.taps, g.axis, false, index)? };
    let wu = usize::try_from(w).map_err(|_| EffectError::Overflow)?.max(1);
    let mut out = Vec::with_capacity(src.len());
    for ((pre_row, s_row), b_row) in src.chunks(wu).zip(straight.chunks(wu)).zip(blurred.chunks(wu)) {
        cancel.check().map_err(|_| EffectError::Canceled)?;
        for ((pre, s), b) in pre_row.iter().zip(s_row).zip(b_row) {
            let ga = (g.intensity * f64::from(b[3])).min(1.0);
            let (rgb, a) = if ga > 0.0 {
                let own = [0usize, 1, 2].map(|k| (g.intensity * f64::from(b[k]) * f64::from(b[3]) / ga).clamp(0.0, 1.0));
                match &g.colours {
                    None => (own, ga),
                    Some(c) => {
                        let m = lerp4(c.a, c.b, c.position(ga));
                        ([m[0], m[1], m[2]].map(|v| v.clamp(0.0, 1.0)), ga * m[3].clamp(0.0, 1.0))
                    }
                }
            } else {
                ([0.0; 3], 0.0)
            };
            if a <= 0.0 {
                out.push(if g.original == GlowOriginal::None { [0.0; 4] } else { widen(*pre) });
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            let light = Rgba::new(rgb.map(|v| v as f32), a as f32);
            let original = Rgba::new([s[0], s[1], s[2]], s[3]);
            let site = PixelSite::default();
            let r = match g.original {
                GlowOriginal::Behind => composite_over(g.mode, original, light, 1.0, site).map_err(param(index))?,
                GlowOriginal::OnTop => composite_over(StudioBlendMode::Normal, light, original, 1.0, site).map_err(param(index))?,
                GlowOriginal::None => light,
            };
            out.push(premultiply([r.rgb[0], r.rgb[1], r.rgb[2], r.a]));
        }
    }
    Ok(out)
}

/// The colour entry over `src` (premultiplied, rows of `w`) as premultiplied texels (module docs).
fn colour(c: &ColourEffect, index: usize, src: &[[f32; 4]], w: usize, cancel: &CancellationToken) -> Result<Vec<[f64; 4]>, EffectError> {
    let prepared: Vec<(PreparedAdjustment, [bool; 3])> =
        c.ops.iter().map(|o| o.op.prepare().map(|p| (p, o.channels)).map_err(param(index))).collect::<Result<_, _>>()?;
    let mut out = Vec::with_capacity(src.len());
    for row in src.chunks(w.max(1)) {
        cancel.check().map_err(|_| EffectError::Canceled)?;
        for p in row {
            let s = unpremultiply(*p);
            if s[3] <= 0.0 {
                out.push(widen(*p));
                continue;
            }
            let input = [s[0], s[1], s[2]];
            let mut adjusted = input;
            for (op, channels) in &prepared {
                let y = op.apply(input).map_err(param(index))?;
                for ((slot, value), on) in adjusted.iter_mut().zip(y).zip(channels) {
                    if *on {
                        *slot = value;
                    }
                }
            }
            #[allow(clippy::cast_possible_truncation)]
            let mixed = [0usize, 1, 2].map(|k| (f64::from(adjusted[k]) * (1.0 - c.blend) + f64::from(input[k]) * c.blend) as f32);
            out.push(premultiply([mixed[0], mixed[1], mixed[2], s[3]]));
        }
    }
    Ok(out)
}

/// The result of a whole-buffer effect over the pre-entry buffer, `None` for a per-texel effect.
fn whole_buffer(
    effect: &SceneEffect,
    index: usize,
    src: &[[f32; 4]],
    w: u32,
    h: u32,
    cancel: &CancellationToken,
) -> Result<Option<Vec<[f64; 4]>>, EffectError> {
    Ok(Some(match effect {
        SceneEffect::Blur { taps, .. } if identity_taps(taps) => src.iter().map(|p| widen(*p)).collect(),
        SceneEffect::DirectionalBlur { length_px, .. } if *length_px <= 0.0 => src.iter().map(|p| widen(*p)).collect(),
        SceneEffect::RadialBlur { amount, .. } if *amount <= 0.0 => src.iter().map(|p| widen(*p)).collect(),
        SceneEffect::FindEdges { blend, .. } if *blend >= 1.0 => src.iter().map(|p| widen(*p)).collect(),
        SceneEffect::Blur { taps, axis, repeat_edges } => {
            let straight = straight_of(src);
            cancel.check().map_err(|_| EffectError::Canceled)?;
            blur_plane(&straight, w, h, taps, *axis, *repeat_edges, index)?.into_iter().map(premultiply).collect()
        }
        SceneEffect::DirectionalBlur { angle_deg, length_px } => {
            let straight = straight_of(src);
            cancel.check().map_err(|_| EffectError::Canceled)?;
            let rect = Rect { x: 0, y: 0, width: w, height: h };
            let limits = BlurLimits { max_bytes: MAX_EFFECT_BLUR_BYTES, max_work: MAX_EFFECT_WORK };
            #[allow(clippy::cast_possible_truncation)]
            let (angle, length) = (angle_deg.rem_euclid(360.0) as f32, *length_px as f32);
            directional_blur_window(SampleRange::Unit, &straight, rect, rect, angle, length, limits)
                .map_err(|e| blur_err(e, index))?
                .into_iter()
                .map(premultiply)
                .collect()
        }
        SceneEffect::RadialBlur { centre, amount, kind } => {
            let straight = straight_of(src);
            cancel.check().map_err(|_| EffectError::Canceled)?;
            let rect = Rect { x: 0, y: 0, width: w, height: h };
            let limits = BlurLimits { max_bytes: MAX_EFFECT_BLUR_BYTES, max_work: MAX_EFFECT_WORK };
            #[allow(clippy::cast_possible_truncation)]
            let (c, a) = ([centre[0] as f32, centre[1] as f32], *amount as f32);
            radial_blur_window(SampleRange::Unit, &straight, rect, rect, c, a, *kind, limits)
                .map_err(|e| blur_err(e, index))?
                .into_iter()
                .map(premultiply)
                .collect()
        }
        SceneEffect::FindEdges { invert, blend } => {
            let straight = straight_of(src);
            cancel.check().map_err(|_| EffectError::Canceled)?;
            let rect = Rect { x: 0, y: 0, width: w, height: h };
            let limits = FilterLimits { max_bytes: MAX_EFFECT_BLUR_BYTES, max_work: MAX_EFFECT_WORK };
            let edges = find_edges_window(&straight, rect, rect, *invert, limits).map_err(|e| filter_err(e, index))?;
            edges
                .into_iter()
                .zip(src)
                .map(|(e, p)| {
                    let (e, p) = (premultiply(e), widen(*p));
                    [0usize, 1, 2, 3].map(|k| e[k] * (1.0 - blend) + p[k] * blend)
                })
                .collect()
        }
        SceneEffect::Glow(g) => glow(g, index, src, w, h, cancel)?,
        SceneEffect::Colour(c) => colour(c, index, src, usize::try_from(w).map_err(|_| EffectError::Overflow)?, cancel)?,
        _ => return Ok(None),
    }))
}

/// Whether `effect` runs as a whole-buffer kernel (it holds extra working surfaces).
fn whole_buffer_kind(effect: &SceneEffect) -> bool {
    match effect {
        SceneEffect::Blur { .. }
        | SceneEffect::DirectionalBlur { .. }
        | SceneEffect::RadialBlur { .. }
        | SceneEffect::FindEdges { .. }
        | SceneEffect::Glow(_)
        | SceneEffect::Colour(_) => true,
        SceneEffect::Composite(c) => whole_buffer_kind(&c.effect),
        _ => false,
    }
}

/// [STU-FX-008] entry blend (module docs): the result's colour replaced by the W3C blended source
/// colour against the pre-entry colour, its alpha kept.
fn entry_blend(mode: StudioBlendMode, pre: [f64; 4], result: [f64; 4]) -> [f64; 4] {
    if result[3] <= 0.0 {
        return result;
    }
    let cs = unpremultiply(narrow(result));
    let cb = unpremultiply(narrow(pre));
    let (cs3, cb3) = ([cs[0], cs[1], cs[2]], [cb[0], cb[1], cb[2]]);
    let b = blend_rgb(mode, cb3, cs3).unwrap_or(cs3);
    let ab = f64::from(cb[3]);
    let mixed = [0usize, 1, 2].map(|k| (1.0 - ab) * f64::from(cs3[k]) + ab * f64::from(b[k]));
    premultiplied([mixed[0], mixed[1], mixed[2], 1.0], result[3])
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
    if let Some(bad) = effects.iter().position(|e| !entry_ok(e) || !mask_ok(e, pixels.len())) {
        return Err(EffectError::Parameter(bad));
    }
    if effects.is_empty() || pixels.is_empty() {
        return Ok(());
    }
    let w = usize::try_from(width).map_err(|_| EffectError::Overflow)?;
    let mut src: Vec<[f32; 4]> = pixels.to_vec();
    for (index, e) in effects.iter().enumerate() {
        src.copy_from_slice(pixels);
        let (inner, comp) = match &e.effect {
            SceneEffect::Composite(c) => (&c.effect, Some(c.as_ref())),
            other => (other, None),
        };
        let whole = whole_buffer(inner, index, &src, width, height, cancel)?;
        for (row, line) in pixels.chunks_mut(w.max(1)).enumerate() {
            cancel.check().map_err(|_| EffectError::Canceled)?;
            #[allow(clippy::cast_precision_loss)]
            let v = row as f64 + 0.5;
            for (col, out) in line.iter_mut().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let u = col as f64 + 0.5;
                let pre = widen(*out);
                let at = row.checked_mul(w).and_then(|r| r.checked_add(col)).ok_or(EffectError::Overflow)?;
                let mut result = match &whole {
                    Some(b) => *b.get(at).ok_or(EffectError::Shape)?,
                    None => evaluate(inner, &src, width, height, u, v, pre),
                };
                let mut mix = e.mix;
                if let Some(c) = comp {
                    if c.mode != StudioBlendMode::Normal {
                        result = entry_blend(c.mode, pre, result);
                    }
                    if let Some(m) = &c.mask {
                        mix *= m.get(at).map_or(0.0, |x| f64::from(*x));
                    }
                }
                *out = narrow(lerp4(pre, result, mix));
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
    if e.effects.iter().any(|x| !entry_ok(x) || !mask_ok(x, n)) {
        return Err(invalid(id, "effect_parameter"));
    }
    let mut surface = content_surface(id, &e.content, e.rect, filter, ctx, cancel, depth, charges, exactness)?;
    if surface.width != e.rect.width || surface.height != e.rect.height {
        return Err(invalid(id, "effect_rect"));
    }
    // The working buffer and the pre-entry copy (16 bytes per pixel each).
    charges.surface(ctx, e.rect.width, e.rect.height)?;
    charges.surface(ctx, e.rect.width, e.rect.height)?;
    // A blur, glow or colour entry holds a straight copy and its result (lane SMB-AE-EFFECT-FAMILIES-2).
    if e.effects.iter().any(|x| whole_buffer_kind(&x.effect)) {
        charges.surface(ctx, e.rect.width, e.rect.height)?;
        charges.surface(ctx, e.rect.width, e.rect.height)?;
    }
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
