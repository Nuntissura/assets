//! Output stage of a plan on the GPU (lanes SMB-RENDER-SCENE-DISPLAY, SMB-PIGMENT-EXTENDED-RANGE),
//! the twin of render-cpu `plan_sink/display.rs`.
//!
//! The stage is pigment's (`render_plan::OutputStage` in the envelope, part of the digest).
//! `SampleRange::Extended`: tiles and solids with any finite colour are admitted and every
//! composite dispatch carries `Params.flags` bit 2 (`EXTENDED_FLAG`), under which the kernel skips
//! its unit clamps and linear dodge is the unclamped `cb + cs` (pigment
//! `blend_math::composite_over_extended`). `DisplayTransform::Clip` (stage 6a) clamps the read-back
//! colour into `[0, 1]` on the host before the sink; the shared-texture path encodes into
//! `Rgba8Unorm` through a shader that clamps every channel to `0..=1` already, so the clip is
//! inherent there. A plan without the stage keeps the unit range and `sample_range_outside_unit`.
//!
//! `GpuBackend::with_display_transform` / `SharedDevice::with_display_transform` are deprecated
//! aliases: a plan whose envelope has the default output stage runs as if it declared
//! `OutputStage { Extended, Some(transform) }` ([`aliased`]).
pub use hsk_studio_pigment::render_plan::{DISPLAY_CLIP_OPERATOR, DisplayTransform};
use hsk_studio_pigment::render_plan::{Exactness, OutputStage, PlanExtensions, SampleRange};

/// Class of the stage-6a clip (pigment's `ExtOperator::DisplayClip` intrinsic; a host clamp).
pub const DISPLAY_CLIP_EXACTNESS: Exactness = Exactness::Exact;
/// `Params.flags` bit of an extended-range composite dispatch (kernel.wgsl / kernel_pixel.wgsl).
pub(crate) const EXTENDED_FLAG: u32 = 4;

/// The envelope a backend with the deprecated alias executes: `None` (use the given envelope)
/// when no alias is set or the envelope already declares an output stage.
pub(crate) fn aliased(
    extensions: Option<&PlanExtensions>,
    alias: Option<DisplayTransform>,
) -> Option<PlanExtensions> {
    let transform = alias?;
    let base = extensions.cloned().unwrap_or_default();
    if !base.output.is_default() {
        return None;
    }
    Some(base.with_output(OutputStage::new(SampleRange::Extended, Some(transform))))
}

/// Is a colour channel admissible in the given range? Unit: finite and in `[0, 1]`; extended:
/// finite.
pub(crate) fn sample_ok(extended: bool, v: f32) -> bool {
    v.is_finite() && (extended || (0.0..=1.0).contains(&v))
}
