//! Layer filters of the CPU backend (lane SMB-RENDER-FILTERS slice 1): gaussian blur, unsharp mask,
//! high pass, median and seeded add noise from the plan envelope (`LayerExtension::filters`). The
//! maths is pigment's single owner `filter_math` (CX-CAP-001): every stage is one call of
//! `filter_window`, which maps a straight-RGBA source plane to the straight RGBA of any output
//! window with each output pixel a function of the source only. This module only chooses the
//! windows and wires them into the chunked compositor; render-cpu keeps no copy of a filter
//! formula, a noise generator or a median. Class Approximate (pigment `FILTER_EXACTNESS`),
//! Photoshop parity NOT_PROVEN.
//!
//! Evaluation. The layer content (colour and alpha of its input node, WITHOUT the layer mask) is
//! stage 0, transparent beyond the content bounds `B0`. Stage `k` (1-based, list order) maps stage
//! `k - 1` to stage `k`; its output is transparent beyond `Bk = grow(B(k-1), growth_k)` (the blur
//! support or the median radius; 0 for the point filters, which return transparent where the
//! source alpha is 0). For a chunk `C` the final window is `W = Bn` clipped to `C` (and to the
//! mask plane when the layer has a mask), and the stage windows are derived backwards,
//! `R(k-1) = grow(Rk, reach_k)` clipped to `B(k-1)`: every pixel a stage reads is inside the
//! window of the stage before it, and everything beyond it is transparent by construction. So the
//! result is the filter chain of the whole layer, bit for bit pigment's `filter_window` applied
//! stage by stage to the whole content (tests/cpu_filters.rs), for every chunking and thread
//! count. Noise uses plan coordinates, so it does not depend on the chunking either.
//!
//! The filtered result is then composited by the normal path: the layer mask (feather, invert,
//! density), Blend-If, mode and `opacity * fill` apply AFTER the filters (the mask does not enter
//! the filter, unlike a layer blur). A mask covers only its plane (`MaskView::stored` is defined
//! inside it), so the part of the filtered result beyond the mask plane (the blur or median
//! spread) is not composited: the window is clipped to the plane.
//!
//! Resource rules: the frame (content over `R0`, its group levels and feather scratch) is the
//! chunk-level effect frame sized in `ExtPrep::frame_shape` for the filtered layers too, pigment's
//! predicted bytes of every stage window are charged to `max_working_bytes` before the first sink
//! call, and a window whose predicted work is above `MAX_EFFECT_WINDOW_WORK` is the typed refusal
//! `filter_work_limit`. A filtered layer inside the content of a layer with effects, a blur or
//! filters is the typed refusal [`FILTER_NESTED`] (the frame is used once per chunk), decided in
//! `prepare` before any sink call.
#![deny(clippy::arithmetic_side_effects)]
use super::extended::{filter_error, stage_reach};
use super::{Eval, Fx, PLayer, RenderError, unsupported};
use crate::px::{area, grow_rect, index_in, intersect, overflow};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::filter_math::filter_window_in;

/// Refusal key: a filter nested where the chunk-level frame is already in use.
pub(super) const FILTER_NESTED: &str = "filter_nested";

impl Eval<'_, '_> {
    /// Does `layer` carry layer filters?
    pub(super) fn is_filtered(&self, layer: &PLayer<'_>) -> bool {
        layer.fx.filter.is_some()
    }

    /// Composes a layer with filters onto `dst` (the running buffer of the chunk `chunk`).
    pub(super) fn filter_layer(
        &self,
        layer: &PLayer<'_>,
        chunk: Rect,
        dst: &mut [[f32; 4]],
        fx: &mut Fx,
    ) -> Result<(), RenderError> {
        let filters = layer
            .fx
            .filter
            .and_then(|i| self.prepared.ext.filter(i))
            .ok_or_else(|| unsupported("plan_shape"))?;
        let Some(bounds) = self.prepared.bound(layer.input) else {
            return Ok(());
        };
        if area(bounds)? == 0 {
            return Ok(());
        }
        // Stage bounds B0..Bn.
        let mut stage = vec![bounds];
        for g in &filters.growth {
            let last = *stage.last().ok_or_else(overflow)?;
            stage.push(grow_rect(last, *g)?);
        }
        let last = *stage.last().ok_or_else(overflow)?;
        let mut window = match intersect(last, chunk)? {
            Some(w) => w,
            None => return Ok(()),
        };
        if let Some(mask) = layer.mask.as_ref() {
            window = match intersect(window, mask.bounds)? {
                Some(w) => w,
                None => return Ok(()),
            };
        }
        // Stage windows R0..Rn, derived backwards.
        let n = filters.ops.len();
        let mut rects = vec![window; n.checked_add(1).ok_or_else(overflow)?];
        for k in (1..=n).rev() {
            let op = filters.ops.get(k.checked_sub(1).ok_or_else(overflow)?);
            let op = op.ok_or_else(overflow)?;
            let out = *rects.get(k).ok_or_else(overflow)?;
            let below = *stage.get(k.checked_sub(1).ok_or_else(overflow)?).ok_or_else(overflow)?;
            let Some(reads) = intersect(grow_rect(out, stage_reach(op)?)?, below)? else {
                return Ok(());
            };
            *rects.get_mut(k.checked_sub(1).ok_or_else(overflow)?).ok_or_else(overflow)? = reads;
        }
        let first = *rects.first().ok_or_else(overflow)?;
        let mut frame = fx
            .frame
            .take()
            .ok_or_else(|| unsupported(FILTER_NESTED))?;
        let filtered = self.filtered_content(layer, &mut frame, &filters.ops, &rects, first);
        fx.frame = Some(frame);
        let filtered = filtered?;
        // The mask, Blend-If, mode and opacity apply to the filtered result.
        self.composite(layer, window, chunk, dst, fx, |x, y, _| {
            Ok((*filtered.get(index_in(window, x, y)?).ok_or_else(overflow)?, 1.0))
        })
    }

    /// The raw content over `first` run through the filter chain; the result covers the last
    /// window of `rects` (row-major).
    fn filtered_content(
        &self,
        layer: &PLayer<'_>,
        frame: &mut super::extended::Frame,
        ops: &[hsk_studio_pigment::filter_math::LayerFilter],
        rects: &[Rect],
        first: Rect,
    ) -> Result<Vec<[f32; 4]>, RenderError> {
        let super::extended::Frame {
            content,
            spare,
            fx: frame_fx,
            filter_limits,
            ..
        } = frame;
        let content = content
            .get_mut(..area(first)?)
            .ok_or_else(|| unsupported("filter_scratch"))?;
        content.fill([0.0; 4]);
        self.frame_content_raw(layer, first, content, spare, frame_fx)?;
        let mut current: Option<Vec<[f32; 4]>> = None;
        for (k, op) in ops.iter().enumerate() {
            let src_rect = *rects.get(k).ok_or_else(overflow)?;
            let out = *rects
                .get(k.checked_add(1).ok_or_else(overflow)?)
                .ok_or_else(overflow)?;
            let next = {
                let src: &[[f32; 4]] = match current.as_deref() {
                    Some(c) => c,
                    None => content,
                };
                filter_window_in(self.prepared.ext.output.sample_range, src, src_rect, out, op, *filter_limits)
                    .map_err(filter_error)?
            };
            current = Some(next);
        }
        match current {
            Some(c) => Ok(c),
            // An empty filter list never reaches the backend (no envelope field): the content.
            None => Ok(content.to_vec()),
        }
    }
}
