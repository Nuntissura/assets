//! Assisted rotoscoping in the composition pipeline (lane SMB-AE-ROTO-WIRING; parity AE-18;
//! STU-CMP-034 v02.217). A layer's Folio roto record (strokes, spans, engine, parameter table,
//! freeze) drives Pigment's roto engine and the shared STU-RAS-121 refine engine to a per-frame
//! matte that the scene path applies as [`crate::composition::SceneContent::Rotoscoped`].
//!
//! Pipeline of [`CompositionTemporalSource::propagate_roto`] (order fixed, deterministic,
//! [STU-CMP-075a]: no parallelism across frames, fixed loop order, the same inputs give the same
//! bits; test `roto_propagation_is_deterministic_and_cached`):
//! 1. Inputs in force: the freeze snapshot when frozen, else the live record (Folio
//!    `StudioAssistedRoto::propagation_inputs`). `engine: learned` refuses
//!    `model_unavailable:roto-learned-propagation` (item 4: no model lane is installed; never a
//!    fallback to `classic`).
//! 2. Admission BEFORE allocation (item 8): span frames within the source, rendered source bytes
//!    ([`RotoLimits::max_source_bytes`]), stroke rasterisation work ([`RotoLimits::max_stroke_work`])
//!    and Pigment's `roto_sequence_admission` (frame count, per-frame working set, aggregate output
//!    bytes, aggregate flow work). A refusal names its limit (typed code), never a partial result.
//! 3. Source frames: the nested composition rendered at each span frame's start tick by the
//!    context's backend (transparent background), straight RGBA in `[0, 1]`.
//! 4. Segmentation (the cache, [`RotoSegmentation`]): per span, the base frame's strokes
//!    (rasterised as capsules of the stroke diameter along the path, pixel centres, source pixels;
//!    `invert_foreground_background` swaps polarity) -> Pigment `roto_base`; then one frame at a
//!    time forward (if `forward`) and backward (if `backward`) with Pigment `roto_propagate` from
//!    the previous frame's matte (item 2: predicted boundary from the previous matte and the
//!    estimated motion, re-segmented in a band of `search_radius` pixels, or Pigment's own 3 px
//!    working band when `search_radius` is absent). A corrective stroke on a later frame in the
//!    direction of travel joins that frame's matte (foreground added, background removed) and the
//!    corrected matte carries on (item 2: "influences every frame in its direction within the
//!    span"). A frame whose prediction is empty is `lost` (item 3) and stays lost until a
//!    corrective foreground stroke recovers it. The cache is reused when its digest (canonical
//!    propagation inputs + the SHA-256 of every source frame) matches.
//! 5. Matte stage (`fine_tune_matte`): the STU-RAS-121 engine with a zero band applies
//!    `matte_feather`, `matte_contrast`, `matte_shift_edge` (pixels / percent as the engine's
//!    `feather` / `contrast` / `shift_edge`), then `matte_reduce_chatter` (Pigment's chatter rule).
//! 6. Refine stage (item 5; active when the record has refine-edge strokes or
//!    `refine_edge_base_radius`): per frame the band is the frame's refine-edge strokes (diameter
//!    widened by twice `refine_edge_base_radius`) or, without strokes on the frame, a band of
//!    `refine_edge_base_radius` around the boundary; a frame with neither is not refined. The
//!    engine's input is thresholded at 0.5 (STU-RAS-121 engine contract), so of the matte stage
//!    only its boundary shift carries into a refined frame. With `fine_tune_refine_edge_matte` the
//!    record's STU-RAS-121 controls and `refine_reduce_chatter` apply; without it the engine runs
//!    at identity controls.
//!
//! Unknown-bounded numerics (STU-RAS-106) are used as given, never clamped; a value the engine's
//! formula is undefined for (a negative radius or feather) is the engine's typed refusal
//! `tool_out_of_range`. Absent numerics are not applied (identity), except where the engine needs
//! a working value: `search_radius` (Pigment's 3 px band) and `decontamination_amount` (the shared
//! engine's 100 %); both are listed in [`RotoReceipt::engine_working_values`]. Rows the engine does
//! not model (quality step, motion threshold / damping, edge detection bias, alternate colour
//! estimation, motion blur, extend where smoothed, decontamination increase radius) are listed in
//! [`RotoReceipt::not_modelled`] when set to a non-default value; the view rows are preview-only.
//!
//! Scene path ([`CompositionTemporalSource::with_roto`]): a frame inside a propagated span shows
//! the layer under its matte; a `lost` frame refuses `roto_frame_lost` (item 3: never silently
//! filled); a frame outside every span (or outside the propagated directions) shows the layer
//! unmatted (Studio reading, After Effects UNVERIFIED); a roto layer without attached mattes refuses
//! [`COMPOSITION_ROTO_NOT_PROPAGATED`]. Freeze (item 6): the frozen snapshot fixes the
//! segmentation inputs, so new strokes and propagation-control changes do not change it, the matte
//! and refine stages stay live, and unfreezing recomputes from the live record; the work area is
//! not stored by Folio, so a freeze covers every span (gap). Every formula is class Approximate
//! (After Effects Roto Brush / Refine Edge UNVERIFIED, D5).
#![deny(clippy::arithmetic_side_effects)]
use super::{CompLayer, CompositionTemporalSource, Content, unsupported};
use crate::CompositeError;
use crate::composition::frame_content::plane_of;
use crate::composition::{RotoMatte, RotoscopedContent, SceneContent, SceneContext, render_scene};
use crate::temporal::TemporalError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    StudioAssistedRoto, StudioRotoEdgeDetection, StudioRotoEngine, StudioRotoPolarity, StudioRotoQuality, StudioRotoSpan,
};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::Exactness;
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::select_tools::ToolError;
use hsk_studio_pigment::select_tools::refine::RefineOptions;
use hsk_studio_pigment::select_tools::roto::{
    RotoLimits as EngineLimits, RotoOptions, RotoStatus, RotoStrokes, roto_base, roto_propagate, roto_sequence_admission,
};
use hsk_studio_pigment::select_tools::roto_refine::{RotoBand, refine_roto_stage};
use hsk_studio_pigment::selection::CoveragePlane;
use std::collections::BTreeMap;
use std::sync::Arc;

/// A roto layer evaluated without attached mattes ([`CompositionTemporalSource::with_roto`]).
pub const COMPOSITION_ROTO_NOT_PROPAGATED: &str = "composition_roto_not_propagated";
/// Code of a `lost` frame shown by the scene path (STU-CMP-034 item 3).
pub const ROTO_FRAME_LOST: &str = "roto_frame_lost";
/// Code of the `learned` engine without a model (item 4).
pub const ROTO_MODEL_UNAVAILABLE: &str = "model_unavailable";
/// The model lane the `learned` refusal names (no such model is installed; nothing is downloaded).
pub const ROTO_LEARNED_MODEL_LANE: &str = "roto-learned-propagation";

/// Bounds of one roto run (STU-CMP-034 item 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RotoLimits {
    /// Bytes of all rendered source frames together (16 per pixel per frame).
    pub max_source_bytes: u64,
    /// Pixel tests of the stroke rasterisation (sum of the clipped capsule boxes).
    pub max_stroke_work: u64,
    /// Pigment's frame count, working set, output bytes and flow work.
    pub engine: EngineLimits,
}
impl Default for RotoLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 1 << 30,
            max_stroke_work: 1 << 32,
            engine: EngineLimits::default(),
        }
    }
}

/// Per-frame state (STU-CMP-034 item 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotoFrameState {
    Base,
    Propagated,
    Lost,
}
impl RotoFrameState {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Propagated => "propagated",
            Self::Lost => "lost",
        }
    }
}

/// One segmented frame of the cache: its state, its binary matte (`None` when lost), the band the
/// graph cut re-labelled and whether a corrective stroke changed it.
#[derive(Clone, Debug, PartialEq)]
pub struct RotoSegmentedFrame {
    pub state: RotoFrameState,
    pub matte: Option<CoveragePlane>,
    pub band_pixels: u64,
    pub corrected: bool,
}

/// The segmentation cache (STU-CMP-034 item 1: "the propagated mattes are a cache, never
/// authority"). A host keeps it in its render cache keyed by `inputs_digest`, outside Folio and
/// outside the document, and passes it back to [`CompositionTemporalSource::propagate_roto`]; a
/// missing or stale cache is recomputed from the records.
#[derive(Clone, Debug, PartialEq)]
pub struct RotoSegmentation {
    /// SHA-256 of the canonical propagation inputs followed by every source frame's colour and
    /// alpha SHA-256.
    pub inputs_digest: [u8; 32],
    pub frame_rect: Rect,
    pub frames: BTreeMap<u64, RotoSegmentedFrame>,
}

/// One frame's final matte.
#[derive(Clone, Debug, PartialEq)]
pub struct RotoMatteFrame {
    pub state: RotoFrameState,
    /// `None` for a lost frame.
    pub matte: Option<Arc<RotoMatte>>,
}

/// The per-frame mattes the scene path applies ([`CompositionTemporalSource::with_roto`]); source
/// frame index -> matte, only for propagated frames.
#[derive(Clone, Debug, PartialEq)]
pub struct RotoMattes {
    pub frame_rect: Rect,
    pub frames: BTreeMap<u64, RotoMatteFrame>,
}

/// Receipt row of one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct RotoFrameReceipt {
    pub frame: u64,
    pub state: RotoFrameState,
    /// Recorded with every propagated frame (item 4).
    pub engine: StudioRotoEngine,
    pub band_pixels: u64,
    pub corrected: bool,
    /// The refine stage ran on this frame.
    pub refined: bool,
}

/// What one run did (item 3: lost frames are reported here; item 9 diagnostics).
#[derive(Clone, Debug, PartialEq)]
pub struct RotoReceipt {
    pub layer_id: String,
    pub inputs_digest: [u8; 32],
    pub frozen: bool,
    pub reused_cache: bool,
    pub frames: Vec<RotoFrameReceipt>,
    /// Set table rows the engine does not model (module docs).
    pub not_modelled: Vec<&'static str>,
    /// Absent rows for which the engine used its own working value (module docs).
    pub engine_working_values: Vec<&'static str>,
    pub exactness: Exactness,
}
impl RotoReceipt {
    /// Frames in state `lost`.
    pub fn lost(&self) -> Vec<u64> {
        self.frames.iter().filter(|f| f.state == RotoFrameState::Lost).map(|f| f.frame).collect()
    }
}

/// One run: the cache, the mattes and the receipt.
#[derive(Clone, Debug, PartialEq)]
pub struct RotoRun {
    pub segmentation: RotoSegmentation,
    pub mattes: Arc<RotoMattes>,
    pub receipt: RotoReceipt,
}

fn tool(layer_id: &str, e: ToolError) -> TemporalError {
    match e {
        ToolError::ModelUnavailable => learned(layer_id),
        other => TemporalError::Source { layer_id: layer_id.to_owned(), code: other.code().to_owned() },
    }
}
fn learned(layer_id: &str) -> TemporalError {
    TemporalError::Source {
        layer_id: layer_id.to_owned(),
        code: format!("{ROTO_MODEL_UNAVAILABLE}:{ROTO_LEARNED_MODEL_LANE}"),
    }
}
fn invalid(layer_id: &str, field: &'static str) -> TemporalError {
    TemporalError::InvalidState { layer_id: layer_id.to_owned(), field }
}
fn overflow() -> TemporalError {
    CompositeError::Overflow.into()
}

/// A stroke as rasterised: diameter and path.
struct Brush<'a> {
    diameter: f64,
    path: &'a [[f64; 2]],
}

/// One capsule: segment ends, radius and the clipped pixel box `[x0, y0, x1, y1)`.
type Capsule = ([f64; 2], [f64; 2], f64, [i64; 4]);

/// The clipped pixel boxes of every capsule of `b` widened by `grow` (segments, or the single
/// point of a one-point path).
fn capsules(b: &Brush<'_>, grow: f64, rect: Rect) -> Vec<Capsule> {
    let r = b.diameter / 2.0 + grow.max(0.0);
    let (w, h) = (f64::from(rect.width), f64::from(rect.height));
    let segs: Vec<([f64; 2], [f64; 2])> = if b.path.len() == 1 {
        b.path.iter().map(|p| (*p, *p)).collect()
    } else {
        b.path.windows(2).filter_map(|s| Some((*s.first()?, *s.get(1)?))).collect()
    };
    segs.into_iter()
        .filter_map(|(p, q)| {
            let x0 = (p[0].min(q[0]) - r - 0.5).floor().max(0.0);
            let y0 = (p[1].min(q[1]) - r - 0.5).floor().max(0.0);
            let x1 = (p[0].max(q[0]) + r + 0.5).ceil().min(w);
            let y1 = (p[1].max(q[1]) + r + 0.5).ceil().min(h);
            #[allow(clippy::cast_possible_truncation)]
            let b = [x0 as i64, y0 as i64, x1 as i64, y1 as i64];
            (x1 > x0 && y1 > y0).then_some((p, q, r, b))
        })
        .collect()
}

/// Pixel tests of rasterising `brushes` (the admission measure).
fn stroke_work(brushes: &[Brush<'_>], grow: f64, rect: Rect) -> Option<u64> {
    let mut work = 0u64;
    for b in brushes {
        for (_, _, _, [x0, y0, x1, y1]) in capsules(b, grow, rect) {
            let area = u64::try_from(x1.checked_sub(x0)?).ok()?.checked_mul(u64::try_from(y1.checked_sub(y0)?).ok()?)?;
            work = work.checked_add(area)?;
        }
    }
    Some(work)
}

/// Coverage of `brushes` over `rect` (module docs: capsules, pixel centres). `None` when empty.
fn stroke_plane(brushes: &[Brush<'_>], grow: f64, rect: Rect) -> Result<Option<CoveragePlane>, TemporalError> {
    if brushes.is_empty() {
        return Ok(None);
    }
    let n = usize::try_from(rect.area()).map_err(|_| overflow())?;
    let w = usize::try_from(rect.width).map_err(|_| overflow())?;
    let mut data = vec![0u16; n];
    for b in brushes {
        for (p, q, r, [x0, y0, x1, y1]) in capsules(b, grow, rect) {
            let (dx, dy) = (q[0] - p[0], q[1] - p[1]);
            let len2 = dx * dx + dy * dy;
            for y in y0..y1 {
                for x in x0..x1 {
                    #[allow(clippy::cast_precision_loss)]
                    let (cx, cy) = (x as f64 + 0.5, y as f64 + 0.5);
                    let t = if len2 > 0.0 { (((cx - p[0]) * dx + (cy - p[1]) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                    let (ex, ey) = (cx - (p[0] + t * dx), cy - (p[1] + t * dy));
                    if ex * ex + ey * ey <= r * r {
                        let (ux, uy) = (usize::try_from(x).map_err(|_| overflow())?, usize::try_from(y).map_err(|_| overflow())?);
                        let at = uy.checked_mul(w).and_then(|v| v.checked_add(ux)).ok_or_else(overflow)?;
                        if let Some(slot) = data.get_mut(at) {
                            *slot = u16::MAX;
                        }
                    }
                }
            }
        }
    }
    Ok(Some(CoveragePlane { bounds: rect, data }))
}

fn painted(p: Option<&CoveragePlane>, i: usize) -> bool {
    p.and_then(|p| p.data.get(i)).is_some_and(|v| *v >= 32768)
}

/// `(matte | fg) & !bg` (a corrective stroke on a propagated frame).
fn correct(matte: Option<&CoveragePlane>, fg: Option<&CoveragePlane>, bg: Option<&CoveragePlane>, rect: Rect) -> Result<CoveragePlane, TemporalError> {
    let n = usize::try_from(rect.area()).map_err(|_| overflow())?;
    let data = (0..n)
        .map(|i| if (painted(matte, i) || painted(fg, i)) && !painted(bg, i) { u16::MAX } else { 0 })
        .collect();
    Ok(CoveragePlane { bounds: rect, data })
}

fn digest_of(bytes: &[u8]) -> [u8; 32] {
    hsk_studio_prism::profile_hash(bytes)
}

impl CompositionTemporalSource {
    /// The composition layer `layer_id` of any reached composition and its nested composition.
    fn roto_layer(&self, layer_id: &str) -> Result<(&CompLayer, &super::Comp, &StudioAssistedRoto), TemporalError> {
        let l = self
            .comps
            .values()
            .flat_map(|c| c.layers.iter())
            .find(|l| l.layer_id == layer_id)
            .ok_or_else(|| CompositeError::DanglingInput(layer_id.to_owned()))?;
        let roto = l.roto.as_deref().ok_or_else(|| invalid(layer_id, "roto"))?;
        let Content::Precomp { composition_id, .. } = &l.content else {
            return Err(unsupported(layer_id, super::COMPOSITION_TIME_SOURCE));
        };
        let nested = self.comps.get(composition_id).ok_or_else(|| CompositeError::DanglingInput(composition_id.clone()))?;
        Ok((l, nested, roto))
    }

    /// Runs the roto pipeline of layer `layer_id` (module docs). `cache` = a previous run's
    /// segmentation (reused when its digest matches); `ctx` renders the source frames.
    pub fn propagate_roto(
        &self,
        layer_id: &str,
        ctx: &SceneContext<'_>,
        cache: Option<&RotoSegmentation>,
        limits: &RotoLimits,
        cancel: &CancellationToken,
    ) -> Result<RotoRun, TemporalError> {
        let (l, nested, roto) = self.roto_layer(layer_id)?;
        let id = layer_id;
        let inputs = roto.propagation_inputs();
        if inputs.engine == StudioRotoEngine::Learned {
            return Err(learned(id));
        }
        let st = l.source.ok_or_else(|| unsupported(id, super::COMPOSITION_TIME_SOURCE))?;
        let frame_count = st.last_frame_index().checked_add(1).ok_or_else(overflow)?;
        let rect = Rect { x: 0, y: 0, width: nested.width, height: nested.height };
        let params = &roto.parameters;
        let live = inputs.parameters;
        // Spans and the frames each covers in its directions.
        let mut runs: Vec<(&StudioRotoSpan, u64, u64)> = Vec::with_capacity(inputs.spans.len());
        let mut total = 0u64;
        for s in inputs.spans {
            if s.out_frame > frame_count {
                return Err(invalid(id, "roto_spans"));
            }
            let lo = if s.backward { s.in_frame } else { s.base_frame };
            let hi = if s.forward { s.out_frame } else { s.base_frame.checked_add(1).ok_or_else(overflow)? };
            total = total.checked_add(hi.checked_sub(lo).ok_or_else(overflow)?).ok_or_else(overflow)?;
            runs.push((s, lo, hi));
        }
        let mut opts = RotoOptions::default();
        if let Some(r) = live.search_radius {
            opts.band_px = r;
        }
        let refine_active = !roto.edge_strokes.is_empty() || params.refine_edge_base_radius.is_some();
        let matte_chatter = if params.fine_tune_matte { params.matte_reduce_chatter.unwrap_or(0.0) } else { 0.0 };
        let refine_chatter = if params.fine_tune_refine_edge_matte { params.refine_reduce_chatter.unwrap_or(0.0) } else { 0.0 };
        // Admission before any allocation (item 8).
        let area = rect.area();
        let source_bytes = area.checked_mul(16).and_then(|b| b.checked_mul(total.checked_add(1)?)).ok_or_else(overflow)?;
        if source_bytes > limits.max_source_bytes {
            return Err(CompositeError::BudgetExceeded.into());
        }
        let brushes: Vec<Brush<'_>> = inputs.strokes.iter().map(|s| Brush { diameter: s.diameter, path: &s.path }).collect();
        let grow = params.refine_edge_base_radius.unwrap_or(0.0);
        let edge: Vec<Brush<'_>> = roto.edge_strokes.iter().map(|s| Brush { diameter: s.diameter, path: &s.path }).collect();
        let work = stroke_work(&brushes, 0.0, rect)
            .zip(stroke_work(&edge, grow, rect))
            .and_then(|(a, b)| a.checked_add(b))
            .ok_or_else(overflow)?;
        if work > limits.max_stroke_work {
            return Err(CompositeError::BudgetExceeded.into());
        }
        let passes = u64::from(params.fine_tune_matte).checked_add(u64::from(refine_active)).ok_or_else(overflow)?;
        let chatter_steps = [matte_chatter, refine_chatter].iter().filter(|c| **c != 0.0).count();
        let chatter_steps = u64::try_from(chatter_steps).map_err(|_| overflow())?.checked_mul(total.checked_mul(2).ok_or_else(overflow)?).ok_or_else(overflow)?;
        let steps = total.saturating_sub(u64::try_from(runs.len()).map_err(|_| overflow())?);
        roto_sequence_admission(rect, total, steps, passes, chatter_steps, &opts, &limits.engine).map_err(|e| tool(id, e))?;
        // Source frames (step 3).
        let mut frames: BTreeMap<u64, RgbaPlane> = BTreeMap::new();
        let mut bytes = inputs.canonical_bytes();
        for (_, lo, hi) in &runs {
            for f in *lo..*hi {
                cancel.check().map_err(|_| TemporalError::Composite(CompositeError::Canceled))?;
                let tick = i64::try_from(f.checked_mul(nested.ticks_per_frame).ok_or_else(overflow)?).map_err(|_| overflow())?;
                let mut scene = self.scene_of(nested, tick, 1, cancel)?;
                scene.background = None;
                let r = render_scene(&scene, ctx, cancel)?;
                bytes.extend_from_slice(&digest_of(&r.colour));
                bytes.extend_from_slice(&digest_of(&r.alpha));
                frames.insert(f, plane_of(id, "roto_frame_out_of_domain", r.width, r.height, &r.colour, &r.alpha)?);
            }
        }
        let digest = digest_of(&bytes);
        // Segmentation (step 4), or the cache.
        let reused = cache.is_some_and(|c| c.inputs_digest == digest && c.frame_rect == rect);
        let segmentation = match cache.filter(|_| reused) {
            Some(c) => c.clone(),
            None => {
                let mut out = BTreeMap::new();
                for (span, lo, hi) in &runs {
                    self.segment_span(id, span, *lo, *hi, &frames, &inputs, rect, &opts, limits, cancel, &mut out)?;
                }
                RotoSegmentation { inputs_digest: digest, frame_rect: rect, frames: out }
            }
        };
        // Matte and refine stages (steps 5-6), per span run.
        let mut mattes = BTreeMap::new();
        let mut receipt_frames = Vec::new();
        for (_, lo, hi) in &runs {
            let idx: Vec<u64> = (*lo..*hi).collect();
            let plane = |f: &u64| frames.get(f).ok_or_else(|| invalid(id, "roto_frames"));
            let run_frames: Vec<RgbaPlane> = idx.iter().map(|f| plane(f).cloned()).collect::<Result<_, _>>()?;
            let empty = CoveragePlane { bounds: rect, data: vec![0; usize::try_from(area).map_err(|_| overflow())?] };
            let seg: Vec<&RotoSegmentedFrame> = idx
                .iter()
                .map(|f| segmentation.frames.get(f).ok_or_else(|| invalid(id, "roto_segmentation")))
                .collect::<Result<_, _>>()?;
            let mut current: Vec<CoveragePlane> = seg.iter().map(|s| s.matte.clone().unwrap_or_else(|| empty.clone())).collect();
            let mut colours: Vec<Option<Vec<[f32; 3]>>> = vec![None; idx.len()];
            if params.fine_tune_matte {
                let refine = RefineOptions {
                    radius: 0.0,
                    smooth: 0.0,
                    feather: params.matte_feather.unwrap_or(0.0),
                    contrast: params.matte_contrast.unwrap_or(0.0),
                    shift_edge: params.matte_shift_edge.unwrap_or(0.0),
                    decontaminate_colours: false,
                    ..RefineOptions::default()
                };
                let bands: Vec<Option<RotoBand<'_>>> = idx.iter().map(|_| Some(RotoBand::Radius(0.0))).collect();
                let out = refine_roto_stage(&run_frames, &current, &bands, &refine, matte_chatter, &opts.flow, &limits.engine, cancel)
                    .map_err(|e| tool(id, e))?;
                current = out.into_iter().map(|r| r.coverage).collect();
            }
            let mut refined = vec![false; idx.len()];
            if refine_active {
                let mut planes: Vec<Option<CoveragePlane>> = Vec::with_capacity(idx.len());
                for f in &idx {
                    let on: Vec<Brush<'_>> = roto
                        .edge_strokes
                        .iter()
                        .filter(|s| s.frame == *f)
                        .map(|s| Brush { diameter: s.diameter, path: &s.path })
                        .collect();
                    planes.push(stroke_plane(&on, grow, rect)?);
                }
                let bands: Vec<Option<RotoBand<'_>>> = planes
                    .iter()
                    .map(|p| match (p, params.refine_edge_base_radius) {
                        (Some(p), _) => Some(RotoBand::Trimap(p)),
                        (None, Some(r)) => Some(RotoBand::Radius(r)),
                        (None, None) => None,
                    })
                    .collect();
                for (slot, b) in refined.iter_mut().zip(&bands) {
                    *slot = b.is_some();
                }
                let tuned = params.fine_tune_refine_edge_matte;
                let c = &params.refine;
                let base = RefineOptions::default();
                let refine = RefineOptions {
                    smooth: if tuned { c.smooth.unwrap_or(0.0) } else { 0.0 },
                    feather: if tuned { c.feather.unwrap_or(0.0) } else { 0.0 },
                    contrast: if tuned { c.contrast.unwrap_or(0.0) } else { 0.0 },
                    shift_edge: if tuned { c.shift_edge.unwrap_or(0.0) } else { 0.0 },
                    decontaminate_colours: tuned && c.decontaminate_colours,
                    decontamination_amount: c.decontamination_amount.unwrap_or(base.decontamination_amount),
                    ..base
                };
                let out = refine_roto_stage(&run_frames, &current, &bands, &refine, refine_chatter, &opts.flow, &limits.engine, cancel)
                    .map_err(|e| tool(id, e))?;
                current = Vec::with_capacity(out.len());
                for (k, r) in out.into_iter().enumerate() {
                    if let Some(slot) = colours.get_mut(k) {
                        *slot = r.colours;
                    }
                    current.push(r.coverage);
                }
            }
            for (k, f) in idx.iter().enumerate() {
                let s = seg.get(k).ok_or_else(|| invalid(id, "roto_segmentation"))?;
                let matte = match s.state {
                    RotoFrameState::Lost => None,
                    _ => Some(Arc::new(RotoMatte {
                        coverage: current.get(k).cloned().ok_or_else(|| invalid(id, "roto_segmentation"))?,
                        colours: colours.get(k).cloned().flatten(),
                    })),
                };
                mattes.insert(*f, RotoMatteFrame { state: s.state, matte });
                receipt_frames.push(RotoFrameReceipt {
                    frame: *f,
                    state: s.state,
                    engine: inputs.engine,
                    band_pixels: s.band_pixels,
                    corrected: s.corrected,
                    refined: refined.get(k).copied().unwrap_or(false) && s.state != RotoFrameState::Lost,
                });
            }
        }
        let mut not_modelled = Vec::new();
        for (set, name) in [
            (live.quality != StudioRotoQuality::Standard, "quality"),
            (live.motion_threshold.is_some(), "motion_threshold"),
            (live.motion_damping.is_some(), "motion_damping"),
            (live.edge_detection != StudioRotoEdgeDetection::Balanced, "edge_detection"),
            (live.use_alternate_colour_estimation, "use_alternate_colour_estimation"),
            (params.use_motion_blur, "use_motion_blur"),
            (params.extend_where_smoothed, "extend_where_smoothed"),
            (params.decontamination_increase_radius.is_some(), "decontamination_increase_radius"),
        ] {
            if set {
                not_modelled.push(name);
            }
        }
        let mut engine_working_values = Vec::new();
        if live.search_radius.is_none() {
            engine_working_values.push("search_radius");
        }
        if params.fine_tune_refine_edge_matte && params.refine.decontaminate_colours && params.refine.decontamination_amount.is_none() {
            engine_working_values.push("refine/decontamination_amount");
        }
        Ok(RotoRun {
            receipt: RotoReceipt {
                layer_id: id.to_owned(),
                inputs_digest: digest,
                frozen: inputs.frozen,
                reused_cache: reused,
                frames: receipt_frames,
                not_modelled,
                engine_working_values,
                exactness: Exactness::Approximate,
            },
            segmentation,
            mattes: Arc::new(RotoMattes { frame_rect: rect, frames: mattes }),
        })
    }

    /// Segmentation of one span over frames `lo..hi` (module docs, step 4).
    #[allow(clippy::too_many_arguments)]
    fn segment_span(
        &self,
        id: &str,
        span: &StudioRotoSpan,
        lo: u64,
        hi: u64,
        frames: &BTreeMap<u64, RgbaPlane>,
        inputs: &hsk_studio_folio::StudioRotoPropagationInputs<'_>,
        rect: Rect,
        opts: &RotoOptions,
        limits: &RotoLimits,
        cancel: &CancellationToken,
        out: &mut BTreeMap<u64, RotoSegmentedFrame>,
    ) -> Result<(), TemporalError> {
        let invert = inputs.parameters.invert_foreground_background;
        let planes = |f: u64| -> Result<(Option<CoveragePlane>, Option<CoveragePlane>), TemporalError> {
            let pick = |want: StudioRotoPolarity| {
                inputs
                    .strokes
                    .iter()
                    .filter(|s| s.frame == f)
                    .filter(|s| (s.polarity == want) != invert)
                    .map(|s| Brush { diameter: s.diameter, path: &s.path })
                    .collect::<Vec<_>>()
            };
            Ok((stroke_plane(&pick(StudioRotoPolarity::Foreground), 0.0, rect)?, stroke_plane(&pick(StudioRotoPolarity::Background), 0.0, rect)?))
        };
        let frame = |f: u64| frames.get(&f).ok_or_else(|| invalid(id, "roto_frames"));
        let base = span.base_frame;
        let (fg, bg) = planes(base)?;
        let Some(fg) = fg else {
            return Err(invalid(id, "roto_base_strokes"));
        };
        let strokes = RotoStrokes { foreground: &fg, background: bg.as_ref() };
        let matte = roto_base(frame(base)?, &strokes, opts, &limits.engine).map_err(|e| tool(id, e))?;
        out.insert(base, RotoSegmentedFrame { state: RotoFrameState::Base, matte: Some(matte.clone()), band_pixels: 0, corrected: false });
        let forward: Vec<(u64, u64)> = (base..hi.saturating_sub(1)).map(|k| (k, k.saturating_add(1))).collect();
        let backward: Vec<(u64, u64)> = (lo.saturating_add(1)..=base).rev().map(|k| (k, k.saturating_sub(1))).collect();
        for order in [forward, backward] {
            let mut prev: Option<CoveragePlane> = Some(matte.clone());
            for (from, to) in order {
                cancel.check().map_err(|_| TemporalError::Composite(CompositeError::Canceled))?;
                let (mut next, mut band) = (None, 0u64);
                if let Some(p) = prev.as_ref().filter(|p| p.data.iter().any(|v| *v >= 32768)) {
                    let pair = if to > from { [frame(from)?.clone(), frame(to)?.clone()] } else { [frame(to)?.clone(), frame(from)?.clone()] };
                    let base_index = usize::from(to < from);
                    let r = roto_propagate(&pair, base_index, p, opts, &limits.engine, cancel).map_err(|e| tool(id, e))?;
                    let step = r.into_iter().find(|f| f.status != RotoStatus::Base).ok_or_else(|| invalid(id, "roto_propagation"))?;
                    band = step.band_pixels;
                    if step.status == RotoStatus::Propagated {
                        next = Some(step.matte);
                    }
                }
                let (cfg, cbg) = planes(to)?;
                let corrected = cfg.is_some() || cbg.is_some();
                if corrected {
                    let c = correct(next.as_ref(), cfg.as_ref(), cbg.as_ref(), rect)?;
                    next = c.data.iter().any(|v| *v != 0).then_some(c);
                }
                let state = if next.is_some() { RotoFrameState::Propagated } else { RotoFrameState::Lost };
                out.insert(to, RotoSegmentedFrame { state, matte: next.clone(), band_pixels: band, corrected });
                prev = next;
            }
        }
        Ok(())
    }

    /// Attaches the mattes of a roto run to layer `layer_id` (module docs, scene path). Unknown
    /// layer `DanglingInput`; a layer without a roto record or mattes over another rect
    /// `InvalidState` (`roto`).
    pub fn with_roto(mut self, layer_id: &str, mattes: Arc<RotoMattes>) -> Result<Self, TemporalError> {
        let (_, nested, _) = self.roto_layer(layer_id)?;
        if mattes.frame_rect != (Rect { x: 0, y: 0, width: nested.width, height: nested.height }) {
            return Err(invalid(layer_id, "roto"));
        }
        self.roto.insert(layer_id.to_owned(), mattes);
        Ok(self)
    }

    /// `content` (source frame `frame` of `l`) under the layer's roto matte (module docs).
    pub(super) fn rotoscope(&self, l: &CompLayer, frame: u64, content: SceneContent) -> Result<SceneContent, TemporalError> {
        if l.roto.is_none() {
            return Ok(content);
        }
        let mattes = self.roto.get(&l.layer_id).ok_or_else(|| unsupported(&l.layer_id, COMPOSITION_ROTO_NOT_PROPAGATED))?;
        match mattes.frames.get(&frame) {
            None => Ok(content),
            Some(RotoMatteFrame { matte: None, .. }) => Err(TemporalError::Source { layer_id: l.layer_id.clone(), code: ROTO_FRAME_LOST.to_owned() }),
            Some(RotoMatteFrame { matte: Some(m), .. }) => Ok(SceneContent::Rotoscoped(Box::new(RotoscopedContent {
                content,
                rect: mattes.frame_rect,
                matte: Arc::clone(m),
            }))),
        }
    }
}
