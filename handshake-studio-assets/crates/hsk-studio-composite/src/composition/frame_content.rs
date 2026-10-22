//! Scene contents of lane SMB-AE-ROTO-WIRING: frame blending ([`FrameBlendedContent`]) and the
//! assisted-roto matte ([`RotoscopedContent`]). Folio-free and motion-free like every scene
//! content; `temporal::composition` builds them from the layer records.
//!
//! Frame blending (STU-MOT-007a "`frame_mix` blends adjacent source frames; `pixel_motion`
//! synthesises intermediate frames by motion estimation"; STU-MOT-045 item 6 "Frame blending
//! governs what happens between source frames under a non-integer remap"): the two frame scenes
//! `a` and `b` are rendered by the context's backend (each surface charged before allocation),
//! both must have the same size and every sample must lie in `[0, 1]` (Pigment's frame-blend domain;
//! anything else is refused `frame_blend_out_of_domain`, never clamped), then Pigment's
//! `frame_mix` / `pixel_motion` produce the frame at `weight_b` (the only owner of the pixel maths,
//! CX-CAP-001). Class Approximate ([`hsk_studio_pigment::frame_blend::FRAME_BLEND_EXACTNESS`];
//! After Effects' Frame Mix / Pixel Motion UNVERIFIED, D5). Both backends share this host step, so
//! a GPU render differs from a CPU render only by the two frame renders.
//!
//! Roto matte (STU-CMP-034 item 5: "The per-frame matte is a matte source of the pipeline of
//! [STU-CMP-033] and enters it at the REFINE stage"): the matte arriving here is the OUTPUT of the
//! refine stage (`temporal::composition` roto pipeline); the inner content is rendered over `rect`
//! (integer placement), its straight alpha is multiplied by the coverage (`u16 / 65535`), and
//! where the refine stage produced decontaminated colours they replace the straight colour of a
//! pixel with coverage above 0. The coverage plane must cover exactly `rect`
//! (`roto_matte_rect`).
#![deny(clippy::arithmetic_side_effects)]
use super::{Charges, SceneComposition, SceneContent, SceneContext, Surface, canceled, content_surface, f32_at, invalid, render_inner};
use crate::CompositeError;
use crate::temporal::TemporalError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{Exactness, ExactnessLedger};
use hsk_studio_pigment::frame_blend::{BlendError, BlendLimits, FRAME_BLEND_EXACTNESS, FlowParams, frame_mix, pixel_motion};
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::selection::CoveragePlane;
use hsk_studio_pigment::transform_math::ResampleFilter;
use std::sync::Arc;

/// Exactness-ledger key of the frame-blend step.
pub const FRAME_BLEND_OPERATOR: &str = "frame_blend";
/// Exactness-ledger key of the roto matte step (segmentation, matte and refine stages are class
/// Approximate; After Effects Roto Brush / Refine Edge UNVERIFIED, D5).
pub const ROTO_MATTE_OPERATOR: &str = "roto_matte";

/// The STU-MOT-007a methods that synthesise a frame (`off` never builds this content).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameBlendMethod {
    FrameMix,
    PixelMotion,
}

/// Two adjacent source frames and the weight of the second (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct FrameBlendedContent {
    pub a: SceneComposition,
    pub b: SceneComposition,
    /// Weight of `b`, in `(0, 1)` when built by the adapter; checked by Pigment (`[0, 1]`).
    pub weight_b: f64,
    pub method: FrameBlendMethod,
    /// Optical-flow parameters of `pixel_motion` (Pigment's defaults unless a caller picks others).
    pub flow: FlowParams,
}

/// One frame's roto matte over the content rect (the refine stage's output).
#[derive(Clone, Debug, PartialEq)]
pub struct RotoMatte {
    pub coverage: CoveragePlane,
    /// Decontaminated straight colours over `coverage.bounds` (row-major), when produced.
    pub colours: Option<Vec<[f32; 3]>>,
}

/// Content under a roto matte (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct RotoscopedContent {
    pub content: SceneContent,
    /// The content's plan rect (the source frame rect).
    pub rect: Rect,
    pub matte: Arc<RotoMatte>,
}

fn blend_err(id: &str, e: BlendError) -> TemporalError {
    match e {
        BlendError::OverBudget { .. } => CompositeError::BudgetExceeded.into(),
        BlendError::Overflow => CompositeError::Overflow.into(),
        BlendError::Canceled => canceled(),
        BlendError::Shape => invalid(id, "frame_blend_shape"),
        BlendError::Parameter => invalid(id, "frame_blend_parameter"),
        BlendError::OutOfDomain => invalid(id, "frame_blend_out_of_domain"),
    }
}

/// A rendered frame as Pigment's straight RGBA plane; refuses samples outside `[0, 1]` (`field`).
pub(crate) fn plane_of(id: &str, field: &'static str, width: u32, height: u32, colour: &[u8], alpha: &[u8]) -> Result<RgbaPlane, TemporalError> {
    let n = usize::try_from(u64::from(width).checked_mul(u64::from(height)).ok_or(CompositeError::Overflow)?)
        .map_err(|_| CompositeError::Overflow)?;
    let mut data = Vec::with_capacity(n);
    for i in 0..n {
        let base = i.checked_mul(3).ok_or(CompositeError::Overflow)?;
        let px = [
            f32_at(colour, base),
            f32_at(colour, base.checked_add(1).ok_or(CompositeError::Overflow)?),
            f32_at(colour, base.checked_add(2).ok_or(CompositeError::Overflow)?),
            f32_at(alpha, i),
        ];
        if px.iter().any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0) {
            return Err(invalid(id, field));
        }
        data.push(px);
    }
    Ok(RgbaPlane { bounds: Rect { x: 0, y: 0, width, height }, data })
}

fn surface_of(plane: &RgbaPlane) -> Result<Surface, TemporalError> {
    let n = plane.data.len();
    let mut colour = Vec::with_capacity(n.checked_mul(12).ok_or(CompositeError::Overflow)?);
    let mut alpha = Vec::with_capacity(n.checked_mul(4).ok_or(CompositeError::Overflow)?);
    for p in &plane.data {
        for c in &p[..3] {
            colour.extend_from_slice(&c.to_le_bytes());
        }
        alpha.extend_from_slice(&p[3].to_le_bytes());
    }
    Ok(Surface {
        width: plane.bounds.width,
        height: plane.bounds.height,
        colour_sha256: hsk_studio_prism::profile_hash(&colour),
        alpha_sha256: hsk_studio_prism::profile_hash(&alpha),
        colour,
        alpha,
    })
}

/// [`super::SceneContent::FrameBlended`] (module docs).
pub(super) fn frame_blended_surface(
    id: &str,
    fb: &FrameBlendedContent,
    ctx: &SceneContext<'_>,
    cancel: &CancellationToken,
    depth: usize,
    charges: &mut Charges,
    exactness: &mut ExactnessLedger,
) -> Result<Surface, TemporalError> {
    if fb.a.width != fb.b.width || fb.a.height != fb.b.height {
        return Err(invalid(id, "frame_blend_shape"));
    }
    if depth >= ctx.limits.max_depth {
        return Err(CompositeError::BudgetExceeded.into());
    }
    let depth = depth.checked_add(1).ok_or(CompositeError::Overflow)?;
    // The output surface is charged with the two frames, before anything is allocated.
    charges.surface(ctx, fb.a.width, fb.a.height)?;
    let mut planes = Vec::with_capacity(2);
    for frame in [&fb.a, &fb.b] {
        cancel.check().map_err(|_| canceled())?;
        let r = render_inner(frame, ctx, cancel, depth, charges)?;
        for op in &r.exactness.operators {
            exactness.record(op.key, op.exactness);
        }
        planes.push(plane_of(id, "frame_blend_out_of_domain", r.width, r.height, &r.colour, &r.alpha)?);
    }
    let (Some(a), Some(b)) = (planes.first(), planes.get(1)) else {
        return Err(invalid(id, "frame_blend_shape"));
    };
    let limits = BlendLimits { max_bytes: ctx.limits.max_surface_bytes, max_work: BlendLimits::DEFAULT.max_work };
    let out = match fb.method {
        FrameBlendMethod::FrameMix => frame_mix(a, b, fb.weight_b, &limits),
        FrameBlendMethod::PixelMotion => pixel_motion(a, b, fb.weight_b, &fb.flow, &limits, cancel),
    }
    .map_err(|e| blend_err(id, e))?;
    exactness.record(FRAME_BLEND_OPERATOR, FRAME_BLEND_EXACTNESS);
    surface_of(&out)
}

/// [`super::SceneContent::Rotoscoped`] (module docs).
#[allow(clippy::too_many_arguments)]
pub(super) fn rotoscoped_surface(
    id: &str,
    r: &RotoscopedContent,
    filter: ResampleFilter,
    ctx: &SceneContext<'_>,
    cancel: &CancellationToken,
    depth: usize,
    charges: &mut Charges,
    exactness: &mut ExactnessLedger,
) -> Result<Surface, TemporalError> {
    let m = &r.matte;
    let n = usize::try_from(r.rect.area()).map_err(|_| CompositeError::Overflow)?;
    let colours_ok = m.colours.as_ref().is_none_or(|c| c.len() == n);
    if r.rect.width == 0 || r.rect.height == 0 || m.coverage.bounds != r.rect || m.coverage.data.len() != n || !colours_ok {
        return Err(invalid(id, "roto_matte_rect"));
    }
    let mut surface = content_surface(id, &r.content, r.rect, filter, ctx, cancel, depth, charges, exactness)?;
    for i in 0..n {
        let c = f64::from(m.coverage.data.get(i).copied().unwrap_or(0)) / 65535.0;
        let at = i.checked_mul(4).ok_or(CompositeError::Overflow)?;
        let a = f64::from(f32_at(&surface.alpha, i));
        #[allow(clippy::cast_possible_truncation)]
        let out = (a * c) as f32;
        if let Some(slot) = surface.alpha.get_mut(at..at.checked_add(4).ok_or(CompositeError::Overflow)?) {
            slot.copy_from_slice(&out.to_le_bytes());
        }
        if let Some(rgb) = m.colours.as_ref().and_then(|v| v.get(i)).filter(|_| c > 0.0) {
            let base = i.checked_mul(12).ok_or(CompositeError::Overflow)?;
            for (k, v) in rgb.iter().enumerate() {
                let o = base.checked_add(k.checked_mul(4).ok_or(CompositeError::Overflow)?).ok_or(CompositeError::Overflow)?;
                if let Some(slot) = surface.colour.get_mut(o..o.checked_add(4).ok_or(CompositeError::Overflow)?) {
                    slot.copy_from_slice(&v.to_le_bytes());
                }
            }
        }
    }
    surface.colour_sha256 = hsk_studio_prism::profile_hash(&surface.colour);
    surface.alpha_sha256 = hsk_studio_prism::profile_hash(&surface.alpha);
    exactness.record(ROTO_MATTE_OPERATOR, Exactness::Approximate);
    Ok(surface)
}
