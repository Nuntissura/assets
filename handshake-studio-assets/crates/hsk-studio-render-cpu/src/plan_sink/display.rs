//! Output stage of a plan (lanes SMB-RENDER-SCENE-DISPLAY, SMB-PIGMENT-EXTENDED-RANGE).
//!
//! The stage is pigment's (`render_plan::OutputStage` in the envelope, part of the digest):
//! `SampleRange::Extended` admits tiles and solids with any finite colour channel and composites
//! them with `blend_math::composite_over_extended` (no unit clamp, STU-COL-161 stage 4);
//! `DisplayTransform::Clip` clamps the composite's colour into `[0, 1]` before the sink (stage 6a,
//! [`clip_chunk`]). A plan without the stage keeps the unit range and its refusal
//! `sample_range_outside_unit`, byte for byte.
//!
//! `Renderer::with_display_transform` (lane SMB-RENDER-SCENE-DISPLAY, which clipped the SOURCES)
//! is a deprecated alias: it executes a plan whose envelope has the default output stage as if the
//! envelope declared `OutputStage { Extended, Some(Clip) }` ([`aliased`]); the receipt digest is
//! that of the envelope actually executed.
use super::{RenderError, f32_at};
use crate::px::{add_u, overflow};
use hsk_studio_pigment::blend_math::{
    BlendError, PixelSite, Rgba, composite_over, composite_over_extended, composite_over_fill,
    fill_inside_blend,
};
pub use hsk_studio_pigment::render_plan::{DISPLAY_CLIP_OPERATOR, DisplayTransform};
use hsk_studio_pigment::render_plan::{
    Exactness, OutputStage, PlanExtensions, SampleRange, StudioBlendMode,
};

/// Class of the stage-6a clip (pigment's `ExtOperator::DisplayClip` intrinsic).
pub const DISPLAY_CLIP_EXACTNESS: Exactness = Exactness::Exact;

/// The envelope a renderer with the deprecated alias executes: `None` (use `extensions` as is)
/// when no alias is set or the envelope already declares an output stage, else a copy declaring
/// extended range and the aliased display transform.
pub(crate) fn aliased(
    extensions: &PlanExtensions,
    alias: Option<DisplayTransform>,
) -> Option<PlanExtensions> {
    let transform = alias?;
    if !extensions.output.is_default() {
        return None;
    }
    Some(
        extensions
            .clone()
            .with_output(OutputStage::new(SampleRange::Extended, Some(transform))),
    )
}

/// Pigment's source-over in the plan's sample range: the unit `composite_over` (unchanged) or
/// `composite_over_extended`.
pub(crate) fn over(
    extended: bool,
    mode: StudioBlendMode,
    backdrop: Rgba,
    source: Rgba,
    opacity: f32,
    site: PixelSite,
) -> Result<Rgba, BlendError> {
    if extended {
        composite_over_extended(mode, backdrop, source, opacity, site)
    } else {
        composite_over(mode, backdrop, source, opacity, site)
    }
}

/// [`over`] with the layer's fill kept apart (lane SMB-PS-ORACLE-PARITY-1): a unit-range layer in
/// one of Photoshop's special-eight modes with fill below 1 goes through pigment
/// `composite_over_fill` (fill inside the blend, D5 PS-B01); every other case is [`over`] at
/// `folded` (the layer's `opacity * fill * coverage`, bits unchanged). `unfilled` is the same product
/// without the fill.
#[allow(clippy::too_many_arguments)]
pub(crate) fn over_fill(
    extended: bool,
    mode: StudioBlendMode,
    backdrop: Rgba,
    source: Rgba,
    folded: f32,
    unfilled: f32,
    fill: f32,
    site: PixelSite,
) -> Result<Rgba, BlendError> {
    if !extended && fill < 1.0 && fill_inside_blend(mode) {
        composite_over_fill(mode, backdrop, source, unfilled, fill, site)
    } else {
        over(extended, mode, backdrop, source, folded, site)
    }
}

/// Is a colour channel admissible as a sample in the given range? Unit: finite and in `[0, 1]`;
/// extended: finite.
pub(crate) fn sample_ok(extended: bool, v: f32) -> bool {
    v.is_finite() && (extended || (0.0..=1.0).contains(&v))
}

/// Stage 6a, view transform (lane SMB-PRISM-VIEW-TRANSFORM): every colour triple of a delivered
/// chunk (`rgb_f32le`) through Prism's `ViewTransform::apply` (alpha is a separate plane and
/// untouched). The composite reaching this point is finite (extended composites refuse a
/// non-finite result typed), so a non-finite triple is refused as `sample_range_outside_unit`.
pub(crate) fn view_chunk(
    colour: &mut [u8],
    view: hsk_studio_prism::ViewTransform,
) -> Result<(), RenderError> {
    let mut at = 0usize;
    while at < colour.len() {
        let g = add_u(at, 4)?;
        let b = add_u(g, 4)?;
        let end = add_u(b, 4)?;
        let rgb = [f32_at(colour, at)?, f32_at(colour, g)?, f32_at(colour, b)?];
        let out = view
            .apply(rgb)
            .map_err(|_| super::unsupported(super::SAMPLE_RANGE_OPERATOR))?;
        let dst = colour.get_mut(at..end).ok_or_else(overflow)?;
        for (chunk, v) in dst.chunks_exact_mut(4).zip(out) {
            chunk.copy_from_slice(&v.to_le_bytes());
        }
        at = end;
    }
    Ok(())
}

/// Stage 6a: clamp every colour sample of a delivered chunk (`rgb_f32le`) into `[0, 1]`.
pub(crate) fn clip_chunk(colour: &mut [u8]) -> Result<(), RenderError> {
    let mut at = 0usize;
    while at < colour.len() {
        let v = f32_at(colour, at)?.clamp(0.0, 1.0);
        let end = add_u(at, 4)?;
        colour
            .get_mut(at..end)
            .ok_or_else(overflow)?
            .copy_from_slice(&v.to_le_bytes());
        at = end;
    }
    Ok(())
}
