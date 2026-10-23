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
//! wording, UNVERIFIED). Placements (lane SMB-PS-ADJUST-STYLES-2; STU-CMP-010a: layers composite
//! bottom-up and the accumulated backdrop is everything below the layer):
//!
//! - shallow in an isolated group: the initial backdrop is the group's start (transparent);
//! - shallow at the plan root: the document base (transparent; Studio has no Background layer, so
//!   Photoshop's "knock out to the Background layer" reading has nothing to reveal: UNVERIFIED);
//! - shallow in a pass-through group: the initial backdrop is the running result of the enclosing
//!   buffer just before the pass-through group begins (a pass-through group composites into that
//!   buffer, so its "start" is that result);
//! - deep: the initial backdrop is the document base (transparent), and the knockout shape also
//!   clears every enclosing group's backdrop: when an isolated group holding deep knockouts with the
//!   accumulated shape `K` ([`knockout_union`]) composites onto its parent, the parent's running
//!   result is first [`knockout_clear`]ed by `K`, and `K` carries on to the next enclosing buffer up
//!   to the root. Inside pass-through groups the layer already composites into the shared buffer.
//!
//! Class `Approximate` for every placement (D5).
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

/// The deep-knockout clear of an enclosing backdrop: `lerp(running, transparent, k)` on
/// premultiplied colour and alpha, so the straight colour is kept and the alpha scales by `1 - k`
/// (a fully cleared pixel is transparent).
pub fn knockout_clear(running: Rgba, k: f32) -> Result<Rgba, BlendError> {
    if !unit(k) {
        return Err(BlendError::OutOfDomain);
    }
    let a = running.a - running.a * k;
    if a <= 0.0 {
        return Ok(Rgba::TRANSPARENT);
    }
    Ok(Rgba {
        rgb: running.rgb,
        a: a.clamp(0.0, 1.0),
    })
}

/// Union of two knockout shapes (`k + s (1 - k)`, the coverage union, exact at `k = 1`): the
/// accumulated deep shape of a buffer after a layer of shape `s`.
pub fn knockout_union(k: f32, s: f32) -> Result<f32, BlendError> {
    if !unit(k) || !unit(s) {
        return Err(BlendError::OutOfDomain);
    }
    Ok((k + s * (1.0 - k)).clamp(0.0, 1.0))
}
