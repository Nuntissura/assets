//! Layer effects wave 1 (drop shadow, colour overlay, solid stroke) of the CPU backend. The maths is
//! pigment's single owner `effect_math` (CX-CAP-001): the matte of every effect and the layer
//! composition order and fade rules come from `render_layer_reference`, which this module calls per
//! tile; render-cpu keeps no copy of a matte, a distance transform or a composition rule.
//!
//! Evaluation (class Approximate, Photoshop parity NOT_PROVEN, D5 cases 12-14). A layer with
//! effects is evaluated per tile `T` of its effect region (the layer content bounds grown by the
//! effect margin, clipped to the chunk). The tile side is `MAX_REFERENCE_SIDE - 2 * margin` at most
//! (pigment's reference planes hold 64 pixels per side), so the frame `F = T + margin` is one
//! reference plane: the content over `F` is evaluated with the chunked compositor itself (layer
//! content, nested group, mask with feather, density and invert), turned into an `EffectPlane`,
//! composed over the current backdrop of `T` by `render_layer_reference`, and `T` is written back.
//! `margin` is `effect_support_px`, the distance the matte at a tile pixel reads, so the matte of
//! `T` equals the matte of the whole layer (pixels beyond the layer bounds are transparent in both).
//!
//! Limits (typed refusals before the first sink call, see `ExtPrep::build` and `prepare`): an
//! effect margin above `MAX_EFFECT_MARGIN` (31 px; larger needs a windowed matte API in pigment),
//! effects together with Blend-If on one layer, and effects inside the content of a layer with
//! effects. The per-worker frame memory is part of the reserved scratch
//! (`ExtPrep::frame_scratch_bytes`).
#![deny(clippy::arithmetic_side_effects)]
use super::extended::{Frame, Fx, effect_error};
use super::{Eval, PLayer, PNode, RenderError, mask_value_clamped, tile_at, unsupported};
use crate::px::{area, end, grow_rect, index_in, intersect, mul_u, offset, overflow, to_i64};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::effect_math::{
    EffectOp, EffectPlane, LayerParams, MAX_REFERENCE_SIDE, render_layer_reference,
};

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
        let Some(region) = intersect(grow_rect(bounds, effect.margin)?, chunk)? else {
            return Ok(());
        };
        // The frame belongs to the chunk-level scratch; inside it there is none, which is how a
        // layer with effects in the content of a layer with effects is refused at run time too.
        let mut frame = fx
            .frame
            .take()
            .ok_or_else(|| unsupported("effects_nested"))?;
        let outcome = self.effect_tiles(
            layer,
            effect.margin,
            &effect.ops,
            region,
            chunk,
            dst,
            &mut frame,
        );
        fx.frame = Some(frame);
        outcome
    }

    #[allow(clippy::too_many_arguments)]
    fn effect_tiles(
        &self,
        layer: &PLayer<'_>,
        margin: usize,
        ops: &[EffectOp],
        region: Rect,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        frame: &mut Frame,
    ) -> Result<(), RenderError> {
        let plane_side = usize::try_from(MAX_REFERENCE_SIDE).map_err(|_| overflow())?;
        let side = plane_side
            .checked_sub(mul_u(margin, 2)?)
            .ok_or_else(overflow)?
            .min(frame.tile_cap);
        if side == 0 {
            return Err(unsupported("effect_margin"));
        }
        let step = to_i64(side)?;
        let (x1, y1) = end(region)?;
        let extent = |start: i64, stop: i64| -> Result<u32, RenderError> {
            stop.checked_sub(start)
                .map(|n| n.min(step))
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(overflow)
        };
        let mut y = region.y;
        while y < y1 {
            let mut x = region.x;
            while x < x1 {
                let tile = Rect {
                    x,
                    y,
                    width: extent(x, x1)?,
                    height: extent(y, y1)?,
                };
                self.effect_tile(layer, margin, ops, tile, chunk, dst, frame)?;
                x = x.checked_add(step).ok_or_else(overflow)?;
            }
            y = y.checked_add(step).ok_or_else(overflow)?;
        }
        Ok(())
    }

    /// One tile: content over the frame, `render_layer_reference` over the backdrop, write back.
    #[allow(clippy::too_many_arguments)]
    fn effect_tile(
        &self,
        layer: &PLayer<'_>,
        margin: usize,
        ops: &[EffectOp],
        tile: Rect,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        frame: &mut Frame,
    ) -> Result<(), RenderError> {
        let frame_rect = grow_rect(tile, margin)?;
        let count = area(frame_rect)?;
        let Frame {
            content,
            spare,
            fx: frame_fx,
            ..
        } = frame;
        let content = content
            .get_mut(..count)
            .ok_or_else(|| unsupported("effect_scratch"))?;
        content.fill([0.0; 4]);
        self.frame_content(layer, frame_rect, content, spare, frame_fx)?;

        let content_plane = EffectPlane::new(
            frame_rect.width,
            frame_rect.height,
            content.iter().map(|p| [p[0], p[1], p[2]]).collect(),
            content.iter().map(|p| p[3]).collect(),
        )
        .map_err(effect_error)?;
        // The backdrop is the running buffer inside the tile; the margin never reaches the result.
        let mut rgb = vec![[0.0_f32; 3]; count];
        let mut alpha = vec![0.0_f32; count];
        let (tx1, ty1) = end(tile)?;
        for y in tile.y..ty1 {
            for x in tile.x..tx1 {
                let d = *dst.get(index_in(chunk, x, y)?).ok_or_else(overflow)?;
                let at = index_in(frame_rect, x, y)?;
                *rgb.get_mut(at).ok_or_else(overflow)? = [d[0], d[1], d[2]];
                *alpha.get_mut(at).ok_or_else(overflow)? = d[3];
            }
        }
        let backdrop = EffectPlane::new(frame_rect.width, frame_rect.height, rgb, alpha)
            .map_err(effect_error)?;
        let result = render_layer_reference(
            &backdrop,
            &content_plane,
            LayerParams {
                mode: layer.mode,
                opacity: layer.layer_opacity,
                fill_opacity: layer.fill_opacity,
            },
            ops,
        )
        .map_err(effect_error)?;
        for y in tile.y..ty1 {
            for x in tile.x..tx1 {
                let column = u32::try_from(offset(x, frame_rect.x)?).map_err(|_| overflow())?;
                let row = u32::try_from(offset(y, frame_rect.y)?).map_err(|_| overflow())?;
                let p = result
                    .pixel(column, row)
                    .ok_or_else(|| unsupported("effect_plane_shape"))?;
                // Alpha can round 1 ulp above 1; keep the buffer inside the blend domain.
                *dst.get_mut(index_in(chunk, x, y)?).ok_or_else(overflow)? =
                    [p.rgb[0], p.rgb[1], p.rgb[2], p.a.min(1.0)];
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
