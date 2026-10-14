//! Layer blur and backdrop blur of the CPU backend (lane SMB-COMPOSITE-NEW-KINDS slice 1). The maths
//! is pigment's single owner `blur_math` (CX-CAP-001): the taps (`LayerBlur::taps`, the shared
//! Gaussian with `sigma = radius / 2`), the separable premultiplied blur of a window
//! (`blur_window`) and the backdrop mix (`backdrop_mix`); this module only chooses the windows and
//! wires them into the chunked compositor. Class Approximate (pigment `BLUR_EXACTNESS`), Figma and
//! Photoshop parity NOT_PROVEN.
//!
//! Layer blur: for the chunk `C`, the window `W` = the layer bounds grown by the support, clipped
//! to `C`; the layer's shape content (content with its mask in the alpha, `frame_content`) is
//! evaluated over `F` = `W` grown by the support clipped to the layer, blurred at `W` and composited
//! with the layer's mode and `opacity * fill`. Transparent beyond the layer, so the result is the
//! blur of the whole layer for every chunking and thread count.
//!
//! Backdrop blur (an isolated group's layer, pigment validation): for the window `W` = the layer
//! bounds clipped to `C`, the group's layers below the blurred layer are re-evaluated over `F` = `W`
//! grown by the support (the backdrop is a per-pixel function of the plan, so `F` agrees with the
//! running buffer on `W`), blurred at `W`, and mixed into the running buffer inside the layer's shape
//! with `k = shape alpha * layer opacity`; the layer then composites normally.
//!
//! Resource rules: the frame (content, group levels, feather scratch) is the chunk-level effect frame
//! sized in `ExtPrep::frame_shape` for the blurred layers too, and pigment's predicted bytes of the
//! largest blur window are part of the reserved scratch, charged to `max_working_bytes` before the
//! first sink call; a window whose predicted work is above `MAX_EFFECT_WINDOW_WORK` is the typed
//! refusal `blur_work_limit`. A blur or effects inside the content of a blurred layer, and layers
//! with effects or a blur below a backdrop blur in its group, are the typed refusal
//! [`BLUR_NESTED`] (the frame is used once per chunk), decided in `prepare` before any sink call.
#![deny(clippy::arithmetic_side_effects)]
use super::extended::{BlurLayer, Frame, Fx, LayerFx, blur_error};
use super::{Eval, PLayer, RenderError, unsupported};
use crate::px::{area, end, grow_rect, index_in, intersect, overflow};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blur_math::{BlurScope, backdrop_mix, blur_window};

/// Refusal key: a blur or effects nested where the chunk-level frame is already in use.
pub(super) const BLUR_NESTED: &str = "blur_nested";

impl Eval<'_, '_> {
    fn blur_of(&self, layer: &PLayer<'_>) -> Option<&BlurLayer> {
        layer.fx.blur.and_then(|i| self.prepared.ext.blur(i))
    }

    /// Does `layer` carry a layer-scope blur?
    pub(super) fn is_layer_blur(&self, layer: &PLayer<'_>) -> bool {
        self.blur_of(layer)
            .is_some_and(|b| b.blur.scope == BlurScope::Layer)
    }

    /// Composes a layer with a layer blur onto `dst` (the running buffer of the chunk `chunk`).
    pub(super) fn blur_layer(
        &self,
        layer: &PLayer<'_>,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        fx: &mut Fx,
    ) -> Result<(), RenderError> {
        let blur = self
            .blur_of(layer)
            .ok_or_else(|| unsupported("plan_shape"))?;
        let Some(bounds) = self.prepared.bound(layer.input) else {
            return Ok(());
        };
        if area(bounds)? == 0 {
            return Ok(());
        }
        let Some(window) = intersect(grow_rect(bounds, blur.support)?, chunk)? else {
            return Ok(());
        };
        let frame_rect = intersect(grow_rect(window, blur.support)?, bounds)?
            .ok_or_else(|| unsupported("plan_shape"))?;
        let mut frame = fx
            .frame
            .take()
            .ok_or_else(|| unsupported(BLUR_NESTED))?;
        let blurred = self.blurred_content(layer, blur, &mut frame, frame_rect, window);
        fx.frame = Some(frame);
        let blurred = blurred?;
        // The shape (mask included) is blurred: composite without the mask, with the layer's
        // mode and `opacity * fill`.
        let plain = PLayer {
            input: layer.input,
            mode: layer.mode,
            opacity: layer.opacity,
            mask: None,
            clipped: false,
            fx: LayerFx::default(),
            layer_opacity: layer.layer_opacity,
            fill_opacity: layer.fill_opacity,
        };
        self.composite(&plain, window, chunk, dst, fx, |x, y, _| {
            Ok((*blurred.get(index_in(window, x, y)?).ok_or_else(overflow)?, 1.0))
        })
    }

    fn blurred_content(
        &self,
        layer: &PLayer<'_>,
        blur: &BlurLayer,
        frame: &mut Frame,
        frame_rect: Rect,
        window: Rect,
    ) -> Result<Vec<[f32; 4]>, RenderError> {
        let Frame {
            content,
            spare,
            fx: frame_fx,
            blur_limits,
            ..
        } = frame;
        let content = content
            .get_mut(..area(frame_rect)?)
            .ok_or_else(|| unsupported("blur_scratch"))?;
        content.fill([0.0; 4]);
        self.frame_content(layer, frame_rect, content, spare, frame_fx)?;
        blur_window(content, frame_rect, window, &blur.taps, *blur_limits).map_err(blur_error)
    }

    /// Backdrop blur of layer `position` of group `group` (its first `position` layers are the
    /// backdrop): mixes the blurred backdrop into `dst` inside the layer's shape. No-op for a
    /// layer without a backdrop blur.
    pub(super) fn backdrop_blur(
        &self,
        group: usize,
        position: usize,
        layer: &PLayer<'_>,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        fx: &mut Fx,
    ) -> Result<(), RenderError> {
        let Some(blur) = self
            .blur_of(layer)
            .filter(|b| b.blur.scope == BlurScope::Backdrop)
        else {
            return Ok(());
        };
        if layer.layer_opacity <= 0.0 {
            return Ok(());
        }
        let Some(window) = self
            .prepared
            .bound(layer.input)
            .map(|b| intersect(b, chunk))
            .transpose()?
            .flatten()
        else {
            return Ok(());
        };
        let frame_rect = grow_rect(window, blur.support)?;
        let mut frame = fx
            .frame
            .take()
            .ok_or_else(|| unsupported(BLUR_NESTED))?;
        let outcome = self.backdrop_window(
            group, position, layer, blur, &mut frame, frame_rect, window, chunk, dst,
        );
        fx.frame = Some(frame);
        outcome
    }

    #[allow(clippy::too_many_arguments)]
    fn backdrop_window(
        &self,
        group: usize,
        position: usize,
        layer: &PLayer<'_>,
        blur: &BlurLayer,
        frame: &mut Frame,
        frame_rect: Rect,
        window: Rect,
        chunk: Rect,
        dst: &mut [[f32; 4]],
    ) -> Result<(), RenderError> {
        let Frame {
            content,
            spare,
            fx: frame_fx,
            blur_limits,
            ..
        } = frame;
        // The backdrop over the frame: the group's layers below, from a transparent start.
        let backdrop = content
            .get_mut(..area(frame_rect)?)
            .ok_or_else(|| unsupported("blur_scratch"))?;
        backdrop.fill([0.0; 4]);
        self.group_upto(group, position, frame_rect, backdrop, spare, frame_fx)?;
        let blurred = blur_window(backdrop, frame_rect, window, &blur.taps, *blur_limits)
            .map_err(blur_error)?;
        // The layer's shape over the window (content alpha times its mask).
        let shape = content
            .get_mut(..area(window)?)
            .ok_or_else(|| unsupported("blur_scratch"))?;
        shape.fill([0.0; 4]);
        self.frame_content(layer, window, shape, spare, frame_fx)?;
        let (x1, y1) = end(window)?;
        for y in window.y..y1 {
            for x in window.x..x1 {
                let w = index_in(window, x, y)?;
                let k = shape.get(w).ok_or_else(overflow)?[3] * layer.layer_opacity;
                if k <= 0.0 {
                    continue;
                }
                let at = index_in(chunk, x, y)?;
                let b = *dst.get(at).ok_or_else(overflow)?;
                let l = *blurred.get(w).ok_or_else(overflow)?;
                *dst.get_mut(at).ok_or_else(overflow)? =
                    backdrop_mix(b, l, k.min(1.0)).map_err(blur_error)?;
            }
        }
        Ok(())
    }
}
