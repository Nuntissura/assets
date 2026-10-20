//! Host side of the layer-effect passes (pigment `effect_math`, wave 1 and wave 2). The maths is
//! pigment's single owner (CX-CAP-001): the split of a size into a hard and a soft part
//! (`shadow_split`), the whole-pixel offset (`shadow_offset`), the effect tent taps
//! (`effect_blur_kernel`, shadows, glows, satin, bevel soften), the Gaussian taps of the bevel height
//! field (`feather_kernel`), the knockout rescale (`knockout_coverage`, FINISH mode 0) and the
//! margin an effect reads (`effect_support_px`) are called here, never redefined. The
//! per-pixel loops of the matte chain are the WGSL transcriptions in `fx.wgsl`, checked by parity
//! tests against `render_layer_reference`.
//!
//! An effect is compiled into a short list of passes (`FxStep`): a source plane, an optional hard
//! disc dilation, an optional separable blur (two passes), an optional shift and an optional
//! combine step that ends in the matte buffer. This module only decides the passes, their buffers
//! and the parameter block; `plan_sink.rs` dispatches them. Every size is checked (V4-01) and the
//! blur kernels are charged to the caller's working-byte limit as a running aggregate (V4-02).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_pigment::coverage_math::feather_kernel;
use hsk_studio_pigment::effect_math::{
    BevelEmbossOp, BevelPart, BevelStyle, BevelTechnique, EffectError, EffectOp, EffectOrder,
    GROW_WEIGHTS, GlowSource, GradientOverlayOp, PatternOverlayOp, SatinOp, StrokePosition,
    effect_blur_kernel, effect_support_px, grow_reach_px, shadow_offset, shadow_split,
};
use hsk_studio_pigment::render_plan::{ExtOperator, StudioBlendMode};

/// Bytes of the uniform block `FxP` in `fx.wgsl`: 16 header words and 751 vec4 of blur taps.
pub(crate) const FXP_BYTES: u64 = 12_080;
const HEADER_WORDS: usize = 16;
/// Blur taps the block can carry: 751 vec4 (pigment's largest kernel is 2 * 1500 + 1 = 3001).
const MAX_TAPS: usize = 3004;
/// Most `taps (or window area) x pixels` one effect pass may cost on the device; the executor
/// shrinks the chunk to stay below it and refuses the plan when even the smallest chunk is over.
pub(crate) const FX_PASS_MAX_OPS: u64 = 1 << 31;

const OP_INIT: u32 = 0;
const OP_DILATE: u32 = 1;
const OP_BLUR_H: u32 = 2;
const OP_BLUR_V: u32 = 3;
const OP_SHIFT: u32 = 4;
const OP_FINISH: u32 = 5;
/// Bevel shading of a height field (lane SMB-PS-ADJUST-STYLES-2; `style::bevel_matte`).
const OP_BEVEL: u32 = 6;
/// Chisel field: capped signed distance to the shape edge (`style::chisel_field`).
const OP_CHISEL: u32 = 7;
/// Photoshop's grow (lane SMB-PS-ORACLE-PARITY-2; pigment `grow`: spread, choke, stroke), the
/// chamfer weights `GROW_WEIGHTS` in the parameter tail.
const OP_GROW: u32 = 8;
/// FINISH mode of the centre stroke ring `clamp(fin + fin2 - 1)` (pigment `stroke_matte`).
const FINISH_RING: u32 = 10;

/// FINISH modes added by lane SMB-PS-ADJUST-STYLES-2 (fx.wgsl): satin `|fin - fin2| a`, satin
/// inverted `(1 - |fin - fin2|) a`, the union `max(fin, a)` of a stroke and the shape.
// FINISH 4 and 5 (`|fin - fin2| a`, `(1 - |fin - fin2|) a`) stay in fx.wgsl; satin compiles to
// its interior forms 8 and 9 since lane SMB-PS-ORACLE-PARITY-1.
const FINISH_UNION: u32 = 6;
/// Interior coverages (lane SMB-PS-ORACLE-PARITY-1, pigment `reference_interior_matte`): `1 - fin`
/// (inner glow from the centre), satin `|fin - fin2|` and inverted satin `1 - |fin - fin2|`, none
/// multiplied by the shape alpha.
const FINISH_ONE_MINUS: u32 = 7;
const FINISH_SATIN_INTERIOR: u32 = 8;
const FINISH_SATIN_INTERIOR_INVERTED: u32 = 9;
/// INIT mode of a plane of ones (the interior coverage of the overlays).
const INIT_ONES: u32 = 4;

/// Buffers a pass reads and writes: three f32 scratch planes and the matte buffer (the mask buffer
/// of the composite kernels, so the matte composites through the existing kernel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Buf {
    T0,
    T1,
    T2,
    Mask,
}

/// One pass of a matte chain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FxStep {
    op: u32,
    mode: u32,
    /// DILATE: `ceil(radius)`; BLUR: half width of the kernel.
    k: u32,
    /// BLUR: index into [`FxKernels`].
    taps: Option<usize>,
    r: f32,
    outside: f32,
    /// FINISH mode 0 (drop-shadow knockout): the layer's `opacity * fill` of pigment
    /// `knockout_coverage`.
    knock: f32,
    dx: i32,
    dy: i32,
    /// BEVEL: the shading constants `(k, light x, light y, light z, flat)` in the parameter tail.
    extra: [f32; 8],
    pub(crate) fin: Buf,
    pub(crate) fin2: Buf,
    pub(crate) fout: Buf,
    clamp_out: bool,
}

impl FxStep {
    const fn new(op: u32, mode: u32) -> Self {
        Self {
            op,
            mode,
            k: 0,
            taps: None,
            r: 0.0,
            outside: 0.0,
            knock: 0.0,
            dx: 0,
            dy: 0,
            extra: [0.0; 8],
            fin: Buf::T1,
            fin2: Buf::T2,
            fout: Buf::T0,
            clamp_out: false,
        }
    }
    fn explicit(mut self, fin: Buf, fin2: Buf, fout: Buf) -> Self {
        self.fin = fin;
        self.fin2 = fin2;
        self.fout = fout;
        self.clamp_out = fout == Buf::Mask;
        self
    }
}

/// Why an effect could not be compiled (mapped to a typed refusal by the caller).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FxError {
    /// A size overflowed.
    Overflow,
    /// A parameter or an effect kind this backend has no passes for.
    Unsupported(&'static str),
    /// The blur kernels exceed the working-byte limit.
    Budget { requested: u64 },
}

fn from_effect(e: EffectError) -> FxError {
    match e {
        EffectError::Overflow => FxError::Overflow,
        // A feature pigment recognises and refuses (gradient glow, glow range, jitter, Precise):
        // its own key, what `admit_extended` reports for it (PLR-006).
        EffectError::Unsupported(feature) => FxError::Unsupported(feature.key()),
        _ => FxError::Unsupported("effect_parameter"),
    }
}

/// Which pigment kernel a blur pass uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlurKind {
    /// `effect_math::effect_blur_kernel` (drop and inner shadow, glows, satin, bevel soften).
    Tent,
    /// `coverage_math::feather_kernel` (bevel height field, chisel soft).
    Gauss,
}

/// The taps of a blur of `blur_px` of `kind` (pigment's kernels).
fn kernel_taps(kind: BlurKind, blur_px: f32) -> Result<Vec<f32>, FxError> {
    match kind {
        BlurKind::Tent => effect_blur_kernel(blur_px).map_err(|_| FxError::Unsupported("effect_size")),
        BlurKind::Gauss => feather_kernel(blur_px).map_err(|_| FxError::Unsupported("effect_size")),
    }
}

/// Blur kernels shared by all effects of a plan, keyed by the kernel kind and the blur radius.
/// `bytes` is the running aggregate charged against the working-byte limit before a kernel is
/// stored.
#[derive(Debug, Default)]
pub(crate) struct FxKernels {
    entries: Vec<((BlurKind, u32), Vec<f32>)>,
    bytes: u64,
}

impl FxKernels {
    /// Index of the kernel of `blur_px` of `kind`, computing it with pigment on first use.
    fn intern(&mut self, kind: BlurKind, blur_px: f32, limit: u64) -> Result<usize, FxError> {
        let key = (kind, blur_px.to_bits());
        if let Some(i) = self.entries.iter().position(|(k, _)| *k == key) {
            return Ok(i);
        }
        let taps = kernel_taps(kind, blur_px)?;
        if taps.len() > MAX_TAPS {
            return Err(FxError::Unsupported("effect_size"));
        }
        let added = u64::try_from(taps.len())
            .ok()
            .and_then(|n| n.checked_mul(4))
            .ok_or(FxError::Overflow)?;
        let total = self.bytes.checked_add(added).ok_or(FxError::Overflow)?;
        if total > limit {
            return Err(FxError::Budget { requested: total });
        }
        self.bytes = total;
        self.entries.push((key, taps));
        Ok(self.entries.len().saturating_sub(1))
    }
    fn taps(&self, index: usize) -> Option<&[f32]> {
        self.entries.get(index).map(|(_, t)| t.as_slice())
    }
}

/// The passes of one effect plus how it composites onto the running result.
#[derive(Debug)]
pub(crate) struct FxEffect {
    pub(crate) steps: Vec<FxStep>,
    pub(crate) colour: [f32; 3],
    pub(crate) mode: StudioBlendMode,
    pub(crate) opacity: f32,
    pub(crate) over_content: bool,
    /// An interior effect: its passes end in the interior coverage (not multiplied by the alpha)
    /// and it composites inside the interior unit.
    pub(crate) interior: bool,
    /// Largest `window area` of any pass (1 for point passes): the per-pixel cost bound.
    pub(crate) window_ops: u64,
    /// Gradient overlay: the colour and stop opacity per pixel come from pigment's gradient
    /// (`GradientOverlayOp::sample_at`), uploaded as the source region; the matte is the alpha.
    pub(crate) gradient: Option<GradientOverlayOp>,
    /// Pattern overlay: the colour and alpha per pixel come from the decoded pattern pixels
    /// (pigment `PatternOverlayOp::sample_at`), uploaded as the source region; the matte is the
    /// alpha.
    pub(crate) pattern: Option<PatternOverlayOp>,
}

/// The effects of one layer and the margin they read around the layer bounds.
#[derive(Debug)]
pub(crate) struct FxLayer {
    pub(crate) effects: Vec<FxEffect>,
    pub(crate) margin: u32,
    /// The stack holds an interior effect (pigment `EffectOp::is_interior`): the content and the
    /// interior effects compose as one unit mixed back by the shape alpha (`interior_mix`).
    pub(crate) interior: bool,
}

/// Choose each pass's buffers: passes alternate between `T0` and `T1`, the last one writes `last`.
fn assign(steps: &mut [FxStep], last: Buf) {
    let n = steps.len();
    let mut cur: Option<Buf> = None;
    for (i, s) in steps.iter_mut().enumerate() {
        let out = if i.saturating_add(1) == n {
            last
        } else if cur == Some(Buf::T0) {
            Buf::T1
        } else {
            Buf::T0
        };
        let fin = cur.unwrap_or(if out == Buf::T0 { Buf::T1 } else { Buf::T0 });
        let fin2 = if out == Buf::T2 { Buf::T0 } else { Buf::T2 };
        *s = s.explicit(fin, fin2, out);
        cur = Some(out);
    }
}

fn window(k: u32) -> Result<u64, FxError> {
    let side = u64::from(k)
        .checked_mul(2)
        .and_then(|v| v.checked_add(1))
        .ok_or(FxError::Overflow)?;
    side.checked_mul(side).ok_or(FxError::Overflow)
}

/// One grow pass (pigment `grow`) by `radius`, samples beyond the region reading `outside`.
fn grow_step(radius: f32, outside: f32) -> FxStep {
    let mut s = FxStep::new(OP_GROW, 0);
    s.r = radius;
    s.k = grow_reach_px(radius);
    s.outside = outside;
    let [w1, w2, w5] = GROW_WEIGHTS;
    s.extra = [w1, w2, w5, 0.0, 0.0, 0.0, 0.0, 0.0];
    s
}

/// The disc dilation of the stroke emboss's stroke matte (pigment `style`, `dilate_unbudgeted`).
fn dilate_step(radius: f32, outside: f32) -> FxStep {
    let mut s = FxStep::new(OP_DILATE, 0);
    s.r = radius;
    s.k = radius.ceil() as u32;
    s.outside = outside;
    s
}

/// grow -> blur -> shift, from the source plane `init_mode` (pigment `matte_chain`),
/// then the optional combine step `finish`. Returns the passes and their per-pixel cost bound.
fn matte_chain(
    kernels: &mut FxKernels,
    limit: u64,
    init_mode: u32,
    outside: f32,
    (grown_px, blur_px): (f32, f32),
    (dx, dy): (i32, i32),
    finish: Option<u32>,
) -> Result<(Vec<FxStep>, u64), FxError> {
    let mut steps = vec![FxStep::new(OP_INIT, init_mode)];
    let mut ops = 1u64;
    if grown_px > 0.0 {
        let s = grow_step(grown_px, outside);
        ops = ops.max(window(s.k)?);
        steps.push(s);
    }
    // The identity kernel [1.0] leaves every sample unchanged (1.0 * v), so no pass is spent on it.
    let taps_len = kernel_taps(BlurKind::Tent, blur_px)?.len();
    if taps_len > 1 {
        let index = kernels.intern(BlurKind::Tent, blur_px, limit)?;
        let half = u32::try_from(taps_len.saturating_sub(1) / 2).map_err(|_| FxError::Overflow)?;
        for op in [OP_BLUR_H, OP_BLUR_V] {
            let mut s = FxStep::new(op, 0);
            s.k = half;
            s.taps = Some(index);
            s.outside = outside;
            steps.push(s);
        }
        ops = ops.max(u64::try_from(taps_len).map_err(|_| FxError::Overflow)?);
    }
    if dx != 0 || dy != 0 {
        let mut s = FxStep::new(OP_SHIFT, 0);
        s.dx = dx;
        s.dy = dy;
        s.outside = outside;
        steps.push(s);
    }
    if let Some(mode) = finish {
        steps.push(FxStep::new(OP_FINISH, mode));
    }
    assign(&mut steps, Buf::Mask);
    Ok((steps, ops))
}

/// Compile one effect of the envelope into passes (the matte chain of pigment's
/// `reference_matte`, one pass per private helper). `layer_of` is the layer's `opacity * fill`,
/// which a knocked-out drop shadow's FINISH step reads (pigment `knockout_coverage`).
pub(crate) fn compile_effect(
    op: &EffectOp,
    kernels: &mut FxKernels,
    limit: u64,
    layer_of: f32,
) -> Result<FxEffect, FxError> {
    let (steps, window_ops) = match op {
        EffectOp::ColourOverlay(_) => {
            let mut steps = vec![FxStep::new(OP_INIT, INIT_ONES)];
            assign(&mut steps, Buf::Mask);
            (steps, 1)
        }
        EffectOp::DropShadow(o) => {
            let offset = shadow_offset(o.angle_deg, o.distance_px).map_err(from_effect)?;
            let (mut steps, ops) = matte_chain(
                kernels,
                limit,
                0,
                0.0,
                shadow_split(o.size_px, o.spread),
                offset,
                o.layer_knocks_out.then_some(0),
            )?;
            for s in steps.iter_mut().filter(|s| s.op == OP_FINISH && s.mode == 0) {
                s.knock = layer_of;
            }
            (steps, ops)
        }
        EffectOp::OuterGlow(o) => {
            // knocked out by the layer like a knocking drop shadow (pigment `knocks_out`)
            let (mut steps, ops) = matte_chain(
                kernels,
                limit,
                0,
                0.0,
                shadow_split(o.size_px, o.spread),
                (0, 0),
                Some(0),
            )?;
            for s in steps.iter_mut().filter(|s| s.op == OP_FINISH && s.mode == 0) {
                s.knock = layer_of;
            }
            (steps, ops)
        }
        EffectOp::InnerShadow(o) => {
            let offset = shadow_offset(o.angle_deg, o.distance_px).map_err(from_effect)?;
            // the interior coverage: the chain without the multiply by the alpha
            matte_chain(
                kernels,
                limit,
                1,
                1.0,
                shadow_split(o.size_px, o.choke),
                offset,
                None,
            )?
        }
        EffectOp::InnerGlow(o) => matte_chain(
            kernels,
            limit,
            1,
            1.0,
            shadow_split(o.size_px, o.choke),
            (0, 0),
            match o.source {
                GlowSource::Edge => None,
                GlowSource::Centre => Some(FINISH_ONE_MINUS),
            },
        )?,
        EffectOp::Stroke(o) => {
            // pigment `stroke_matte`: outside = grow(a) knocked out (FINISH 0 with the layer's
            // opacity * fill), inside = grow(1 - a) (interior coverage), centre = the ring
            let w = match o.position {
                StrokePosition::Centre => o.size_px / 2.0,
                _ => o.size_px,
            };
            if w <= 0.0 {
                (Vec::new(), 1)
            } else {
                match o.position {
                    StrokePosition::Outside => {
                        let (mut steps, ops) =
                            matte_chain(kernels, limit, 0, 0.0, (w, 0.0), (0, 0), Some(0))?;
                        for s in steps.iter_mut().filter(|s| s.op == OP_FINISH && s.mode == 0) {
                            s.knock = layer_of;
                        }
                        (steps, ops)
                    }
                    StrokePosition::Inside => {
                        matte_chain(kernels, limit, 1, 1.0, (w, 0.0), (0, 0), None)?
                    }
                    StrokePosition::Centre => {
                        let (g_out, g_in) = (grow_step(w, 0.0), grow_step(w, 1.0));
                        let ops = window(g_out.k)?;
                        let steps = vec![
                            FxStep::new(OP_INIT, 0).explicit(Buf::T1, Buf::T2, Buf::T0),
                            g_out.explicit(Buf::T0, Buf::T1, Buf::T2),
                            FxStep::new(OP_INIT, 1).explicit(Buf::T1, Buf::T2, Buf::T0),
                            g_in.explicit(Buf::T0, Buf::T2, Buf::T1),
                            FxStep::new(OP_FINISH, FINISH_RING).explicit(Buf::T1, Buf::T2, Buf::Mask),
                        ];
                        (steps, ops)
                    }
                }
            }
        }
        EffectOp::Satin(o) => satin_steps(o, kernels, limit)?,
        EffectOp::BevelEmboss(o) => bevel_steps(o, kernels, limit)?,
        EffectOp::GradientOverlay(_) | EffectOp::PatternOverlay(_) => {
            let mut steps = vec![FxStep::new(OP_INIT, INIT_ONES)];
            assign(&mut steps, Buf::Mask);
            (steps, 1)
        }
        // An effect kind this backend has no passes for: pigment's operator key for it, what
        // shared admission reports (`ExtOperator::Effect(kind).key()`).
        _ => return Err(FxError::Unsupported(ExtOperator::Effect(op.kind()).key())),
    };
    Ok(FxEffect {
        steps,
        colour: op.colour(),
        mode: op.mode(),
        opacity: op.opacity(),
        over_content: op.placement() == EffectOrder::OverContent,
        interior: op.is_interior(),
        window_ops,
        gradient: match op {
            EffectOp::GradientOverlay(o) => Some(*o),
            _ => None,
        },
        pattern: match op {
            EffectOp::PatternOverlay(o) => Some(*o),
            _ => None,
        },
    })
}

/// Separable blur passes `from -> via -> to` (none for the identity kernel: the result is then
/// `from`). Returns the buffer holding the result and the pass cost.
fn blur_into(
    steps: &mut Vec<FxStep>,
    kernels: &mut FxKernels,
    limit: u64,
    (kind, blur_px): (BlurKind, f32),
    outside: f32,
    (from, via, to): (Buf, Buf, Buf),
) -> Result<(Buf, u64), FxError> {
    let taps_len = kernel_taps(kind, blur_px)?.len();
    if taps_len <= 1 {
        return Ok((from, 1));
    }
    let index = kernels.intern(kind, blur_px, limit)?;
    let half = u32::try_from(taps_len.saturating_sub(1) / 2).map_err(|_| FxError::Overflow)?;
    for (op, fin, fout) in [(OP_BLUR_H, from, via), (OP_BLUR_V, via, to)] {
        let mut s = FxStep::new(op, 0).explicit(fin, Buf::T2, fout);
        s.k = half;
        s.taps = Some(index);
        s.outside = outside;
        steps.push(s);
    }
    Ok((to, u64::try_from(taps_len).map_err(|_| FxError::Overflow)?))
}

/// Satin (`style::satin_matte`): the blurred alpha shifted by `+o` and by `-o`, `|plus - minus|`
/// (inverted when asked) times the alpha.
fn satin_steps(
    o: &SatinOp,
    kernels: &mut FxKernels,
    limit: u64,
) -> Result<(Vec<FxStep>, u64), FxError> {
    let (dx, dy) = shadow_offset(o.angle_deg, o.distance_px).map_err(from_effect)?;
    let mut steps = vec![FxStep::new(OP_INIT, 0).explicit(Buf::T1, Buf::T2, Buf::T0)];
    let (blurred, ops) = blur_into(
        &mut steps,
        kernels,
        limit,
        (BlurKind::Tent, o.size_px),
        0.0,
        (Buf::T0, Buf::T1, Buf::T0),
    )?;
    let minus = (
        dx.checked_neg().ok_or(FxError::Overflow)?,
        dy.checked_neg().ok_or(FxError::Overflow)?,
    );
    for ((sx, sy), out) in [((dx, dy), Buf::T1), (minus, Buf::T2)] {
        let other = if out == Buf::T2 { Buf::T1 } else { Buf::T2 };
        let mut s = FxStep::new(OP_SHIFT, 0).explicit(blurred, other, out);
        s.dx = sx;
        s.dy = sy;
        steps.push(s);
    }
    // the interior coverage (satin is an interior effect): no multiply by the alpha
    let finish = if o.invert {
        FINISH_SATIN_INTERIOR_INVERTED
    } else {
        FINISH_SATIN_INTERIOR
    };
    steps.push(FxStep::new(OP_FINISH, finish).explicit(Buf::T1, Buf::T2, Buf::Mask));
    Ok((steps, ops))
}

/// One stroke half (pigment `stroke_half`) written to `out`.
fn stroke_half_into(steps: &mut Vec<FxStep>, w: f32, outside: bool, out: Buf) -> Result<u64, FxError> {
    let (init, beyond, finish) = if outside { (2, 0.0, 0) } else { (3, 1.0, 1) };
    let d = dilate_step(w, beyond).explicit(Buf::T0, Buf::T2, Buf::T1);
    let ops = window(d.k)?;
    steps.push(FxStep::new(OP_INIT, init).explicit(Buf::T1, Buf::T2, Buf::T0));
    steps.push(d);
    steps.push(FxStep::new(OP_FINISH, finish).explicit(Buf::T1, Buf::T0, out));
    Ok(ops)
}

/// The stroke matte of a stroke emboss into `T2` (pigment's stroke rule); `None` when the stroke
/// is empty (the bevel then draws nothing).
fn stroke_into_t2(
    steps: &mut Vec<FxStep>,
    size_px: f32,
    position: StrokePosition,
) -> Result<Option<u64>, FxError> {
    Ok(match position {
        StrokePosition::Outside | StrokePosition::Inside if size_px <= 0.0 => None,
        StrokePosition::Outside => Some(stroke_half_into(steps, size_px, true, Buf::T2)?),
        StrokePosition::Inside => Some(stroke_half_into(steps, size_px, false, Buf::T2)?),
        StrokePosition::Centre => {
            let w = size_px / 2.0;
            if w <= 0.0 {
                return Ok(None);
            }
            let ops = stroke_half_into(steps, w, true, Buf::T2)?;
            // the inside half, min(outside + inside, 1) into T0, copied to T2
            let d = dilate_step(w, 1.0).explicit(Buf::T0, Buf::T2, Buf::T1);
            steps.push(FxStep::new(OP_INIT, 3).explicit(Buf::T1, Buf::T2, Buf::T0));
            steps.push(d);
            steps.push(FxStep::new(OP_FINISH, 3).explicit(Buf::T1, Buf::T2, Buf::T0));
            steps.push(FxStep::new(OP_SHIFT, 0).explicit(Buf::T0, Buf::T1, Buf::T2));
            Some(ops)
        }
    })
}

/// Bevel & emboss, one part (`style::bevel_matte`): the shape (the union of shape and stroke for
/// stroke emboss) -> the height field (smooth blur, chisel distance, chisel soft blur) -> the
/// shading with the region rule -> the optional soften blur.
fn bevel_steps(
    o: &BevelEmbossOp,
    kernels: &mut FxKernels,
    limit: u64,
) -> Result<(Vec<FxStep>, u64), FxError> {
    let s = &o.shape;
    let mut steps = Vec::new();
    let mut ops = 1u64;
    // 1. the shape into T0
    if s.style == BevelStyle::StrokeEmboss {
        let Some(stroke) = s.stroke else {
            return Ok((Vec::new(), 1));
        };
        let Some(stroke_ops) = stroke_into_t2(&mut steps, stroke.size_px, stroke.position)? else {
            return Ok((Vec::new(), 1));
        };
        ops = ops.max(stroke_ops);
        steps.push(FxStep::new(OP_FINISH, FINISH_UNION).explicit(Buf::T2, Buf::T1, Buf::T0));
    } else {
        steps.push(FxStep::new(OP_INIT, 0).explicit(Buf::T1, Buf::T2, Buf::T0));
    }
    // 2. the height field and its value beyond the frame
    let (field, beyond) = match s.technique {
        BevelTechnique::Smooth => {
            let (f, cost) = blur_into(
                &mut steps,
                kernels,
                limit,
                (BlurKind::Gauss, s.size_px),
                0.0,
                (Buf::T0, Buf::T1, Buf::T0),
            )?;
            ops = ops.max(cost);
            (f, 0.0)
        }
        BevelTechnique::ChiselHard | BevelTechnique::ChiselSoft => {
            let mut c = FxStep::new(OP_CHISEL, 0).explicit(Buf::T0, Buf::T2, Buf::T1);
            c.r = s.size_px;
            c.k = (s.size_px + 0.5).ceil() as u32;
            ops = ops.max(window(c.k)?);
            steps.push(c);
            let mut f = Buf::T1;
            if s.technique == BevelTechnique::ChiselSoft {
                let (b, cost) = blur_into(
                    &mut steps,
                    kernels,
                    limit,
                    (BlurKind::Gauss, s.size_px / 4.0),
                    -1.0,
                    (Buf::T1, Buf::T0, Buf::T1),
                )?;
                f = b;
                ops = ops.max(cost);
            }
            (f, if s.size_px <= 0.0 { 0.0 } else { -1.0 })
        }
    };
    // 3. the shading constants exactly as pigment derives them (binary64), passed as binary32
    let scale = s.depth / 100.0 * s.size_px * if s.up { 1.0 } else { -1.0 };
    let (sa, ca) = f64::from(s.angle_deg).to_radians().sin_cos();
    let (sl, cl) = f64::from(s.altitude_deg).to_radians().sin_cos();
    let style: u32 = match s.style {
        BevelStyle::InnerBevel => 0,
        BevelStyle::OuterBevel => 2,
        BevelStyle::Emboss => 4,
        BevelStyle::PillowEmboss => 6,
        BevelStyle::StrokeEmboss => 8,
    };
    let mode = match o.part {
        BevelPart::Highlight => style,
        BevelPart::Shadow => style | 1,
    };
    let soften = s.soften_px > 0.0;
    let out = match (soften, field) {
        (false, _) => Buf::Mask,
        (true, Buf::T0) => Buf::T1,
        (true, _) => Buf::T0,
    };
    let mut b = FxStep::new(OP_BEVEL, mode).explicit(field, Buf::T2, out);
    b.outside = beyond;
    b.extra = [
        scale,
        (cl * ca) as f32,
        (-cl * sa) as f32,
        sl as f32,
        sl as f32,
        0.0,
        0.0,
        0.0,
    ];
    steps.push(b);
    // 4. soften: the part matte blurred (zero beyond the frame), clamped into the mask
    if soften {
        let via = if out == Buf::T0 { Buf::T1 } else { Buf::T0 };
        let (done, cost) = blur_into(
            &mut steps,
            kernels,
            limit,
            (BlurKind::Tent, s.soften_px),
            0.0,
            (out, via, Buf::Mask),
        )?;
        ops = ops.max(cost);
        if done != Buf::Mask {
            // an identity kernel: copy the part matte into the mask
            steps.push(FxStep::new(OP_SHIFT, 0).explicit(done, Buf::T2, Buf::Mask));
        }
    }
    Ok((steps, ops))
}

/// The pass writing the shape alpha (the content frame's alpha) into the mask buffer: the mix
/// weight of the interior unit (lane SMB-PS-ORACLE-PARITY-1).
pub(crate) fn shape_alpha_step() -> FxStep {
    FxStep::new(OP_INIT, 0).explicit(Buf::T1, Buf::T2, Buf::Mask)
}

/// Margin in pixels an effect reads around the layer bounds (pigment `effect_support_px`).
pub(crate) fn margin_of(op: &EffectOp) -> Result<u32, FxError> {
    effect_support_px(op).map_err(from_effect)
}

/// The parameter block of one pass: `frame_w` is the stride of the content frame, `region` the
/// `[x, y, width, height]` of the pass inside it.
pub(crate) fn params_bytes(
    step: &FxStep,
    kernels: &FxKernels,
    frame_w: u32,
    region: [u32; 4],
) -> Result<Vec<u8>, FxError> {
    let taps = match step.taps {
        Some(i) => kernels.taps(i).ok_or(FxError::Unsupported("effect_plan"))?,
        None if step.op == OP_BEVEL || step.op == OP_GROW => &step.extra[..],
        None => &[],
    };
    let taps_n = u32::try_from(taps.len()).map_err(|_| FxError::Overflow)?;
    let words: [[u8; 4]; HEADER_WORDS] = [
        frame_w.to_ne_bytes(),
        region[0].to_ne_bytes(),
        region[1].to_ne_bytes(),
        region[2].to_ne_bytes(),
        region[3].to_ne_bytes(),
        step.op.to_ne_bytes(),
        step.mode.to_ne_bytes(),
        step.k.to_ne_bytes(),
        taps_n.to_ne_bytes(),
        u32::from(step.clamp_out).to_ne_bytes(),
        step.dx.to_ne_bytes(),
        step.dy.to_ne_bytes(),
        step.r.to_ne_bytes(),
        step.outside.to_ne_bytes(),
        step.knock.to_ne_bytes(),
        [0; 4],
    ];
    let total = usize::try_from(FXP_BYTES).map_err(|_| FxError::Overflow)?;
    let mut out = vec![0u8; total];
    for (slot, word) in out.chunks_exact_mut(4).zip(words) {
        slot.copy_from_slice(&word);
    }
    let tail = out
        .get_mut(HEADER_WORDS.saturating_mul(4)..)
        .ok_or(FxError::Overflow)?;
    for (slot, tap) in tail.chunks_exact_mut(4).zip(taps) {
        slot.copy_from_slice(&tap.to_ne_bytes());
    }
    Ok(out)
}
