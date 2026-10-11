//! Host side of the layer-effect passes (pigment `effect_math`, wave 1 and wave 2). The maths is
//! pigment's single owner (CX-CAP-001): the split of a size into a hard and a soft part
//! (`shadow_split`), the whole-pixel offset (`shadow_offset`), the Gaussian taps (`feather_kernel`)
//! and the margin an effect reads (`effect_support_px`) are called here, never redefined. The
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
    EffectError, EffectOp, EffectOrder, GlowSource, StrokePosition, effect_support_px,
    shadow_offset, shadow_split,
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
    dx: i32,
    dy: i32,
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
            dx: 0,
            dy: 0,
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

/// Blur kernels shared by all effects of a plan, keyed by the blur radius. `bytes` is the running
/// aggregate charged against the working-byte limit before a kernel is stored.
#[derive(Debug, Default)]
pub(crate) struct FxKernels {
    entries: Vec<(u32, Vec<f32>)>,
    bytes: u64,
}

impl FxKernels {
    /// Index of the kernel of `blur_px`, computing it with pigment's `feather_kernel` on first use.
    fn intern(&mut self, blur_px: f32, limit: u64) -> Result<usize, FxError> {
        let key = blur_px.to_bits();
        if let Some(i) = self.entries.iter().position(|(k, _)| *k == key) {
            return Ok(i);
        }
        let taps = feather_kernel(blur_px).map_err(|_| FxError::Unsupported("effect_size"))?;
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
    /// Largest `window area` of any pass (1 for point passes): the per-pixel cost bound.
    pub(crate) window_ops: u64,
}

/// The effects of one layer and the margin they read around the layer bounds.
#[derive(Debug)]
pub(crate) struct FxLayer {
    pub(crate) effects: Vec<FxEffect>,
    pub(crate) margin: u32,
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

fn dilate_step(radius: f32, outside: f32) -> FxStep {
    let mut s = FxStep::new(OP_DILATE, 0);
    s.r = radius;
    s.k = radius.ceil() as u32;
    s.outside = outside;
    s
}

/// grow (hard disc) -> blur -> shift, from the source plane `init_mode` (pigment `matte_chain`),
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
        let s = dilate_step(grown_px, outside);
        ops = ops.max(window(s.k)?);
        steps.push(s);
    }
    // The identity kernel [1.0] leaves every sample unchanged (1.0 * v), so no pass is spent on it.
    let taps_len = feather_kernel(blur_px)
        .map_err(|_| FxError::Unsupported("effect_size"))?
        .len();
    if taps_len > 1 {
        let index = kernels.intern(blur_px, limit)?;
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

/// One half of a stroke (`stroke_half`): `outside` = region dilated by `w` minus the alpha,
/// otherwise the alpha minus the region eroded by `w`. Width zero is an empty matte (no passes).
fn stroke_half(w: f32, outside: bool) -> Result<(Vec<FxStep>, u64), FxError> {
    if w <= 0.0 {
        return Ok((Vec::new(), 1));
    }
    let (init, beyond, finish) = if outside { (2, 0.0, 0) } else { (3, 1.0, 1) };
    let d = dilate_step(w, beyond);
    let ops = window(d.k)?;
    let mut steps = vec![
        FxStep::new(OP_INIT, init),
        d,
        FxStep::new(OP_FINISH, finish),
    ];
    assign(&mut steps, Buf::Mask);
    Ok((steps, ops))
}

/// Compile one effect of the envelope into passes (the matte chain of pigment's
/// `reference_matte`, one pass per private helper).
pub(crate) fn compile_effect(
    op: &EffectOp,
    kernels: &mut FxKernels,
    limit: u64,
) -> Result<FxEffect, FxError> {
    let (steps, window_ops) = match op {
        EffectOp::ColourOverlay(_) => {
            let mut steps = vec![FxStep::new(OP_INIT, 0)];
            assign(&mut steps, Buf::Mask);
            (steps, 1)
        }
        EffectOp::DropShadow(o) => {
            let offset = shadow_offset(o.angle_deg, o.distance_px).map_err(from_effect)?;
            matte_chain(
                kernels,
                limit,
                0,
                0.0,
                shadow_split(o.size_px, o.spread),
                offset,
                o.layer_knocks_out.then_some(0),
            )?
        }
        EffectOp::OuterGlow(o) => matte_chain(
            kernels,
            limit,
            0,
            0.0,
            shadow_split(o.size_px, o.spread),
            (0, 0),
            None,
        )?,
        EffectOp::InnerShadow(o) => {
            let offset = shadow_offset(o.angle_deg, o.distance_px).map_err(from_effect)?;
            matte_chain(
                kernels,
                limit,
                1,
                1.0,
                shadow_split(o.size_px, o.choke),
                offset,
                Some(1),
            )?
        }
        EffectOp::InnerGlow(o) => matte_chain(
            kernels,
            limit,
            1,
            1.0,
            shadow_split(o.size_px, o.choke),
            (0, 0),
            Some(match o.source {
                GlowSource::Edge => 1,
                GlowSource::Centre => 2,
            }),
        )?,
        EffectOp::Stroke(o) => match o.position {
            StrokePosition::Outside => stroke_half(o.size_px, true)?,
            StrokePosition::Inside => stroke_half(o.size_px, false)?,
            StrokePosition::Centre => {
                let half = o.size_px / 2.0;
                if half <= 0.0 {
                    (Vec::new(), 1)
                } else {
                    // outside half into T2, inside half, then min(outside + inside, 1)
                    let (d_out, d_in) = (dilate_step(half, 0.0), dilate_step(half, 1.0));
                    let ops = window(d_out.k)?;
                    let steps = vec![
                        FxStep::new(OP_INIT, 2).explicit(Buf::T1, Buf::T2, Buf::T0),
                        d_out.explicit(Buf::T0, Buf::T2, Buf::T1),
                        FxStep::new(OP_FINISH, 0).explicit(Buf::T1, Buf::T0, Buf::T2),
                        FxStep::new(OP_INIT, 3).explicit(Buf::T1, Buf::T2, Buf::T0),
                        d_in.explicit(Buf::T0, Buf::T2, Buf::T1),
                        FxStep::new(OP_FINISH, 3).explicit(Buf::T1, Buf::T2, Buf::Mask),
                    ];
                    (steps, ops)
                }
            }
        },
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
        window_ops,
    })
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
        [0; 4],
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
