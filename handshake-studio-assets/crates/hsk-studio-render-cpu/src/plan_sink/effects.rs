//! Layer effects waves 1 and 2 (drop shadow, colour overlay, solid stroke; inner shadow, outer
//! glow, inner glow with the colour fill, Softer technique, full range, no jitter) of the CPU
//! backend. The inner kinds read the inverted layer alpha (1 beyond the layer) and are clipped to
//! the layer alpha by pigment; this module has no kind-specific code (test cpu_effects_fx2
//! `wave2_parity_with_reference_across_chunkings_and_workers`). The maths is
//! pigment's single owner `effect_math` (CX-CAP-001): the matte of every effect and the layer
//! composition order and fade rules come from `effect_math::window::render_layer_window`, which
//! this module calls once per chunk; render-cpu keeps no copy of a matte, a distance transform or a
//! composition rule.
//!
//! Evaluation (class Approximate, Photoshop parity NOT_PROVEN, D5 cases 12-14). A layer with
//! effects is evaluated for the window `W` of each chunk: the layer content bounds grown by the
//! stack margin (`stack_support_px`, the distance any matte of the stack reads), clipped to the
//! chunk. The geometry is `WindowGeometry::uncut(bounds, margin, W)`, so the result is the matte of
//! the whole layer, not of a cut plane, for every chunking and thread count. The layer alpha reaches
//! pigment through [`FrameAlpha`], an `AlphaSource` over a content frame: the layer's shape (content
//! over `W` grown by the margin and clipped to the layer bounds, including a nested isolated group,
//! the mask with feather, density and invert) evaluated by the chunked compositor itself. Pigment
//! asks it for single-row rectangles inside the layer, the window grown by the margin, and
//! nothing else; a request outside the frame is the typed refusal `effect_source`.
//!
//! The dissolve blend mode uses plan coordinates with seed 0 (`site_origin` (0, 0)), which equals
//! `render_layer_reference` on a reference plane placed at the plan origin; the result does not
//! depend on the chunking (test `parity_with_reference_across_chunkings_and_threads`).
//!
//! Resource rules: the largest window's predicted peak bytes (`layer_window_cost`), the two window
//! input arrays and the frame are part of the reserved scratch (`ExtPrep::frame_scratch_bytes`) and
//! charged to `max_working_bytes` before the first sink call; pigment checks the prediction against
//! its limits before it reads or allocates. A window whose predicted work is above
//! `MAX_EFFECT_WINDOW_WORK` is a typed refusal (test `work_limit_and_hostile_geometry_stay_typed`).
//! Effects together with Blend-If on one layer, and effects inside the content of a layer with
//! effects, are typed refusals as well (see `prepare`).
#![deny(clippy::arithmetic_side_effects)]
use super::extended::{Frame, Fx, window_error};
use super::{Eval, PLayer, PNode, RenderError, mask_value_clamped, tile_at, unsupported};
use crate::px::{area, end, grow_rect, index_in, intersect, overflow};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::effect_math::window::{
    AlphaSource, WindowError, WindowGeometry, WindowPlane, render_layer_window,
};
use hsk_studio_pigment::effect_math::{EffectOp, LayerParams};

/// Fills `content` over `region` (a sub-rectangle of `frame`) from `pixel(x, y)`.
fn fill_region(
    content: &mut [[f32; 4]],
    frame: Rect,
    region: Rect,
    mut pixel: impl FnMut(i64, i64) -> Result<[f32; 4], RenderError>,
) -> Result<(), RenderError> {
    let (x1, y1) = end(region)?;
    for y in region.y..y1 {
        for x in region.x..x1 {
            *content
                .get_mut(index_in(frame, x, y)?)
                .ok_or_else(overflow)? = pixel(x, y)?;
        }
    }
    Ok(())
}

/// The layer shape alpha (content alpha times the layer mask) over a content frame, read row by
/// row for pigment's windowed evaluation.
struct FrameAlpha<'a> {
    frame: Rect,
    content: &'a [[f32; 4]],
}

impl AlphaSource for FrameAlpha<'_> {
    fn read_alpha(&self, rect: Rect, out: &mut [f32]) -> Result<(), WindowError> {
        let refused = |_| WindowError::Source;
        if !self
            .frame
            .contains(rect)
            .map_err(|_| WindowError::Overflow)?
        {
            return Err(WindowError::Source);
        }
        let width = usize::try_from(rect.width).map_err(|_| WindowError::Overflow)?;
        let (_, y1) = rect.end().map_err(|_| WindowError::Overflow)?;
        let mut done = 0_usize;
        for y in rect.y..y1 {
            let start = index_in(self.frame, rect.x, y).map_err(refused)?;
            let stop = start.checked_add(width).ok_or(WindowError::Overflow)?;
            let next = done.checked_add(width).ok_or(WindowError::Overflow)?;
            let row = self.content.get(start..stop).ok_or(WindowError::Source)?;
            let dst = out.get_mut(done..next).ok_or(WindowError::Source)?;
            for (d, p) in dst.iter_mut().zip(row) {
                *d = p[3];
            }
            done = next;
        }
        if done == out.len() {
            Ok(())
        } else {
            Err(WindowError::Source)
        }
    }
}

impl Eval<'_, '_> {
    /// Composes a layer with effects onto `dst` (the running buffer of the chunk `chunk`).
    pub(super) fn effect_layer(
        &self,
        layer: &PLayer<'_>,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        fx: &mut Fx,
    ) -> Result<(), RenderError> {
        let effect = layer
            .fx
            .effect
            .and_then(|i| self.prepared.ext.effect(i))
            .ok_or_else(|| unsupported("plan_shape"))?;
        let Some(bounds) = self.prepared.bounds[layer.input] else {
            return Ok(());
        };
        if area(bounds)? == 0 {
            return Ok(());
        }
        let Some(window) = intersect(grow_rect(bounds, effect.margin)?, chunk)? else {
            return Ok(());
        };
        // The frame belongs to the chunk-level scratch; inside it there is none, which is how a
        // layer with effects in the content of a layer with effects is refused at run time too.
        let mut frame = fx
            .frame
            .take()
            .ok_or_else(|| unsupported("effects_nested"))?;
        let outcome = self.effect_window(
            layer,
            effect.margin,
            &effect.ops,
            bounds,
            window,
            chunk,
            dst,
            &mut frame,
        );
        fx.frame = Some(frame);
        outcome
    }

    /// One window: the content over the frame, `render_layer_window` over the backdrop of the
    /// window, written back into the running buffer.
    #[allow(clippy::too_many_arguments)]
    fn effect_window(
        &self,
        layer: &PLayer<'_>,
        margin: usize,
        ops: &[EffectOp],
        bounds: Rect,
        window: Rect,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        frame: &mut Frame,
    ) -> Result<(), RenderError> {
        let Frame {
            content,
            spare,
            fx: frame_fx,
            limits,
        } = frame;
        // The alpha reads of the window stay inside the window grown by the margin and the layer.
        let frame_rect = intersect(grow_rect(window, margin)?, bounds)?
            .ok_or_else(|| unsupported("plan_shape"))?;
        let content = content
            .get_mut(..area(frame_rect)?)
            .ok_or_else(|| unsupported("effect_scratch"))?;
        content.fill([0.0; 4]);
        self.frame_content(layer, frame_rect, content, spare, frame_fx)?;

        let count = area(window)?;
        let mut content_rgb = vec![[0.0_f32; 3]; count];
        let mut backdrop_rgb = vec![[0.0_f32; 3]; count];
        let mut backdrop_alpha = vec![0.0_f32; count];
        let (wx1, wy1) = end(window)?;
        let (fx1, fy1) = end(frame_rect)?;
        let mut at = 0_usize;
        for y in window.y..wy1 {
            for x in window.x..wx1 {
                let d = *dst.get(index_in(chunk, x, y)?).ok_or_else(overflow)?;
                *backdrop_rgb.get_mut(at).ok_or_else(overflow)? = [d[0], d[1], d[2]];
                *backdrop_alpha.get_mut(at).ok_or_else(overflow)? = d[3];
                if x >= frame_rect.x && x < fx1 && y >= frame_rect.y && y < fy1 {
                    let c = *content
                        .get(index_in(frame_rect, x, y)?)
                        .ok_or_else(overflow)?;
                    *content_rgb.get_mut(at).ok_or_else(overflow)? = [c[0], c[1], c[2]];
                }
                at = at.checked_add(1).ok_or_else(overflow)?;
            }
        }
        let backdrop =
            WindowPlane::new(window, backdrop_rgb, backdrop_alpha).map_err(window_error)?;
        let geometry = WindowGeometry::uncut(
            bounds,
            u32::try_from(margin).map_err(|_| overflow())?,
            window,
        )
        .map_err(window_error)?
        .with_site_origin((0, 0));
        let source = FrameAlpha {
            frame: frame_rect,
            content,
        };
        let result = render_layer_window(
            &source,
            &geometry,
            &content_rgb,
            &backdrop,
            LayerParams {
                mode: layer.mode,
                opacity: layer.layer_opacity,
                fill_opacity: layer.fill_opacity,
            },
            ops,
            *limits,
        )
        .map_err(window_error)?;
        let plane = &result.plane;
        let mut at = 0_usize;
        for y in window.y..wy1 {
            for x in window.x..wx1 {
                let rgb = plane.rgb().get(at).ok_or_else(overflow)?;
                let alpha = *plane.alpha().get(at).ok_or_else(overflow)?;
                // Alpha can round 1 ulp above 1; keep the buffer inside the blend domain.
                *dst.get_mut(index_in(chunk, x, y)?).ok_or_else(overflow)? =
                    [rgb[0], rgb[1], rgb[2], alpha.min(1.0)];
                at = at.checked_add(1).ok_or_else(overflow)?;
            }
        }
        Ok(())
    }

    /// The layer's shape over `frame_rect`: straight colour and the content alpha times the layer
    /// mask (feather, invert and density), before opacity and fill. Pixels outside the content
    /// bounds stay transparent.
    fn frame_content(
        &self,
        layer: &PLayer<'_>,
        frame_rect: Rect,
        content: &mut [[f32; 4]],
        spare: &mut [Vec<[f32; 4]>],
        fx: &mut Fx,
    ) -> Result<(), RenderError> {
        let Some(region) = self.prepared.bounds[layer.input]
            .map(|b| intersect(b, frame_rect))
            .transpose()?
            .flatten()
        else {
            return Ok(());
        };
        match &self.prepared.nodes[layer.input] {
            PNode::Group { isolated: true, .. } => {
                self.group(layer.input, frame_rect, content, spare, fx)?;
            }
            PNode::Tile { planes, rect } => {
                fill_region(content, frame_rect, region, |x, y| {
                    tile_at(planes, *rect, x, y)
                })?;
            }
            PNode::Solid { rgba } => {
                fill_region(content, frame_rect, region, |_, _| Ok(*rgba))?;
            }
            PNode::Vector { path, rgba } => {
                fill_region(content, frame_rect, region, |x, y| {
                    Ok([rgba[0], rgba[1], rgba[2], rgba[3] * path.coverage(x, y)])
                })?;
            }
            PNode::Group {
                isolated: false, ..
            }
            | PNode::Unused => return Err(unsupported("plan_shape")),
        }
        if layer.mask.is_none() {
            return Ok(());
        }
        let feathered = self.feather(layer, region, frame_rect, fx)?;
        let plane = feathered.then_some(fx.plane.as_slice());
        let (x1, y1) = end(region)?;
        for y in region.y..y1 {
            for x in region.x..x1 {
                let at = index_in(frame_rect, x, y)?;
                let mask = mask_value_clamped(layer, plane, x, y, at)?;
                let pixel = content.get_mut(at).ok_or_else(overflow)?;
                pixel[3] *= mask;
            }
        }
        Ok(())
    }
}
