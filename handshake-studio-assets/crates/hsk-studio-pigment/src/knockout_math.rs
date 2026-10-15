//! Knockout compositing (lane SMB-PS-ADJUST-STYLES; STU-RAS-041 "knockout" on `StudioLayer`,
//! STU-VEC-153 knockout state): single owner (CX-CAP-001) of the per-pixel rule backends apply to a
//! layer whose `LayerExtension.knockout` is `Shallow`.
//!
//! Rule (Studio reading after the PDF 1.4 knockout-group model, ISO 32000-1 11.4.8: an element of
//! a knockout group is composited with the group's INITIAL backdrop, not with the elements below
//! it, and the result replaces the running result in proportion to the element's shape):
//!
//! - `S = composite_over(mode, initial, source, opacity * fill)`: the layer over the group's initial
//!   backdrop (transparent for an isolated group);
//! - `K = shape`: the layer's shape coverage (content alpha x coverage x mask x LAYER opacity; fill
//!   opacity does not reduce the knockout, which is how a layer at fill 0 punches a hole);
//! - result `= lerp(running, S, K)` on PREMULTIPLIED colour and on alpha.
//!
//! Photoshop's knockout is unpublished; shallow = "to the end of the layer group" (Adobe help
//! wording, UNVERIFIED). Deep knockout (to the Background layer) has no Studio definition and is
//! refused by [`crate::render_plan::PlanExtensions::validate`]. Class `Approximate` (D5).
use crate::blend_math::{BlendError, Exactness, PixelSite, Rgba, StudioBlendMode, composite_over};

/// Class of the knockout rule (Photoshop parity UNVERIFIED, D5).
pub const KNOCKOUT_EXACTNESS: Exactness = Exactness::Approximate;

fn unit(v: f32) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}

/// One pixel of a shallow-knockout layer: `running` is the group's result below the layer,
/// `initial` the group's initial backdrop, `source` the layer content (straight, alpha = content
/// alpha), `opacity` = layer opacity x fill, `shape` = the knockout shape coverage in `[0, 1]`.
pub fn knockout_over(
    mode: StudioBlendMode,
    running: Rgba,
    initial: Rgba,
    source: Rgba,
    opacity: f32,
    shape: f32,
    site: PixelSite,
) -> Result<Rgba, BlendError> {
    if !unit(shape) {
        return Err(BlendError::OutOfDomain);
    }
    let s = composite_over(mode, initial, source, opacity, site)?;
    let a = running.a + (s.a - running.a) * shape;
    if a <= 0.0 {
        return Ok(Rgba::TRANSPARENT);
    }
    let mut rgb = [0.0f32; 3];
    for (i, out) in rgb.iter_mut().enumerate() {
        let pr = running.rgb[i] * running.a;
        let ps = s.rgb[i] * s.a;
        *out = ((pr + (ps - pr) * shape) / a).clamp(0.0, 1.0);
    }
    Ok(Rgba {
        rgb,
        a: a.clamp(0.0, 1.0),
    })
}
