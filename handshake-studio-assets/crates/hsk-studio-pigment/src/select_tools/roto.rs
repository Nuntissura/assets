//! Assisted rotoscoping (lane SMB-AE-ROTO-TIMEREMAP slice 2; parity AE-18 "Roto Brush"): a
//! base-frame stroke mask becomes a segmented matte, which is propagated frame by frame forward
//! and backward through a footage sequence.
//!
//! Spec basis (module 14): [STU-CMP-034] "DECLARED GAP -- rotoscoping. Mask-path animation
//! ([STU-CMP-025]) is the manual rotoscoping mechanism ... An ASSISTED rotoscoping capability --
//! propagating a hand-drawn boundary across frames by tracking edges -- is NOT specified here ...
//! until it is specified an implementer builds the manual path and does not improvise a propagation
//! algorithm"; [STU-CMP-100] "GAP -- assisted rotoscoping ... boundary propagation is not
//! specified". This module was dispatched by the lead with Operator-approved scope expansion
//! (lane SMB-AE-ROTO-TIMEREMAP, 2026-10-20) and fills the gap with a published, cited method
//! rather than an improvised one; the spec conflict is recorded in the lane notes for a spec
//! enrichment of STU-CMP-100. Everything here is class Approximate; After Effects Roto Brush
//! behaviour is UNVERIFIED (D5).
//!
//! Method (clean-room from the papers, no code read):
//! - Base frame: interactive graph cut, Boykov and Jolly, ICCV 2001 (hard seeds from the strokes,
//!   regional term from colour histograms) with the GrabCut n-links and model re-estimation of
//!   Rother, Kolmogorov and Blake, SIGGRAPH 2004 -- the one Pigment graph-cut engine of
//!   [`super::quick`] (`segment`), not a copy. Foreground strokes are hard foreground, background
//!   strokes hard background (background wins where both are painted); the other side's initial
//!   model is the ROI's 2-pixel border ring (the ROI is the stroke bounds grown by
//!   `base_margin_px`).
//! - Propagation k -> k +- 1, after the shape-prior idea of Bai, Wang, Simons and Sapiro, "Video
//!   SnapCut: Robust Video Object Cutout Using Localized Classifiers", SIGGRAPH 2009 (warp the
//!   previous segmentation by optical flow, keep the confident interior and exterior, re-segment an
//!   uncertainty band), simplified to one global colour model: (1) dense flow from the new frame to
//!   the previous one ([`crate::frame_blend`], Horn-Schunck coarse to fine); (2) the previous mask
//!   is sampled bilinearly along the flow and thresholded at 0.5 (predicted mask); (3) pixels
//!   farther than `band_px` (exact Euclidean distance minus 0.5) from the predicted boundary are
//!   hard foreground / background, the band is free; (4) colour models = histograms of the
//!   previous frame's foreground / background in the band's ROI plus the new frame's hard pixels;
//!   (5) the graph cut labels the band. An empty result is reported [`RotoStatus::Lost`] and stays
//!   lost. No occlusion handling, no localized classifiers, no learned model.
//! - A learned propagation model ([`RotoEngine::Learned`]) is not installed and nothing is
//!   downloaded: it refuses `model_unavailable` (same route as object / subject / sky select).
//!
//! Hostile inputs (V4-01/02): every frame is validated, sizes must agree, options are checked
//! (never clamped), and the output bytes of the whole sequence plus the aggregate flow work are
//! refused against [`RotoLimits`] before any allocation.
#![deny(clippy::arithmetic_side_effects)]
use super::quick::{GraphCutOptions, Hard, WORK_BYTES_PER_PIXEL, bin, check_options, counts, segment};
use super::refine::edt_grid;
use super::{RgbaImage, ToolError, ToolLimits, index_in};
use crate::frame_blend::{BlendError, BlendLimits, FlowParams, bilinear, flow_admit, flow_gray, luma};
use crate::retouch::RgbaPlane;
use crate::selection::CoveragePlane;
use crate::{Error, Rect};
use hsk_studio_accord::CancellationToken;

/// Propagation engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RotoEngine {
    /// Flow-warped shape prior plus graph cut (module docs).
    #[default]
    GraphCut,
    /// A learned segmentation / propagation model: not installed, refuses `model_unavailable`.
    Learned,
}

/// Roto options. Nothing is clamped; out-of-domain values refuse.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RotoOptions {
    pub engine: RotoEngine,
    pub graph: GraphCutOptions,
    /// ROI growth around the base strokes (`>= 0`, finite).
    pub base_margin_px: f64,
    /// Half-width of the re-segmented band around the predicted boundary (`>= 0`, finite).
    pub band_px: f64,
    pub flow: FlowParams,
}
impl Default for RotoOptions {
    fn default() -> Self {
        Self {
            engine: RotoEngine::GraphCut,
            graph: GraphCutOptions::default(),
            base_margin_px: 16.0,
            band_px: 3.0,
            flow: FlowParams::DEFAULT,
        }
    }
}

/// Budgets of one roto run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RotoLimits {
    /// Most frames one propagation may carry.
    pub max_frames: usize,
    /// Bytes of all output mattes together (2 bytes per pixel per frame).
    pub max_output_bytes: u64,
    /// Graph-cut working set per frame.
    pub tool: ToolLimits,
    /// Optical-flow bytes per step and aggregate work over the whole sequence.
    pub blend: BlendLimits,
}
impl Default for RotoLimits {
    fn default() -> Self {
        Self {
            max_frames: 4096,
            max_output_bytes: 1 << 30,
            tool: ToolLimits::default(),
            blend: BlendLimits::DEFAULT,
        }
    }
}

/// How a frame's matte was obtained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotoStatus {
    Base,
    Propagated,
    /// The segmentation came out empty (the object left or was lost); later frames stay empty.
    Lost,
}

/// One frame of a propagated roto matte (binary coverage over the frame bounds).
#[derive(Clone, Debug, PartialEq)]
pub struct RotoFrame {
    pub index: usize,
    pub matte: CoveragePlane,
    pub status: RotoStatus,
    /// Pixels the graph cut re-labelled (the uncertainty band).
    pub band_pixels: u64,
}

/// Base-frame strokes: painted foreground and optional background coverage (`>= 0.5` = painted).
#[derive(Clone, Copy, Debug)]
pub struct RotoStrokes<'a> {
    pub foreground: &'a CoveragePlane,
    pub background: Option<&'a CoveragePlane>,
}

pub(crate) fn blend_err(e: BlendError) -> ToolError {
    match e {
        BlendError::Parameter => ToolError::OutOfRange,
        BlendError::OverBudget { .. } => Error::BudgetExceeded.into(),
        BlendError::Overflow => Error::Overflow.into(),
        BlendError::Canceled => Error::Canceled.into(),
        BlendError::Shape | BlendError::OutOfDomain => Error::InvalidInput.into(),
    }
}

fn check(opts: &RotoOptions) -> Result<(), ToolError> {
    if opts.engine == RotoEngine::Learned {
        return Err(ToolError::ModelUnavailable);
    }
    check_options(&opts.graph)?;
    for v in [opts.base_margin_px, opts.band_px] {
        if !v.is_finite() {
            return Err(Error::Nonfinite.into());
        }
        if v < 0.0 {
            return Err(ToolError::OutOfRange);
        }
    }
    if opts.base_margin_px > 1.0e9 || opts.band_px > 1.0e9 {
        return Err(Error::Overflow.into());
    }
    Ok(())
}

pub(crate) fn check_frame(f: &RgbaPlane) -> Result<(usize, usize), ToolError> {
    if f.bounds.width == 0 || f.bounds.height == 0 {
        return Err(Error::InvalidInput.into());
    }
    f.validate().map_err(|_| Error::InvalidInput)?;
    Ok((f.bounds.width as usize, f.bounds.height as usize))
}

/// Premultiplied working copy of `roi` (inside `frame.bounds`).
fn image(frame: &RgbaPlane, roi: Rect, limits: &ToolLimits) -> Result<RgbaImage, Error> {
    let n = limits.admit(roi.area(), RgbaImage::BYTES_PER_PIXEL)?;
    let (ex, ey) = roi.end()?;
    let mut px = Vec::with_capacity(n);
    for y in roi.y..ey {
        for x in roi.x..ex {
            let s = frame.index(x, y).and_then(|i| frame.data.get(i)).copied().unwrap_or([0.0; 4]);
            let a = f64::from(s[3]);
            px.push([f64::from(s[0]) * a, f64::from(s[1]) * a, f64::from(s[2]) * a, a]);
        }
    }
    Ok(RgbaImage { bounds: roi, px })
}

/// Row-major coordinates `(x, y)` of index `i` in a grid of width `w` with origin `o`.
fn at(o: Rect, w: usize, i: usize) -> (i64, i64) {
    let (dx, dy) = (i.checked_rem(w.max(1)).unwrap_or(0), i.checked_div(w.max(1)).unwrap_or(0));
    (o.x.saturating_add(dx as i64), o.y.saturating_add(dy as i64))
}

/// `r` grown by `m` on every side, clipped to `clip`.
fn grow(r: Rect, m: i64, clip: Rect) -> Result<Rect, Error> {
    let (ex, ey) = r.end()?;
    let (cx, cy) = clip.end()?;
    let x0 = r.x.saturating_sub(m).max(clip.x);
    let y0 = r.y.saturating_sub(m).max(clip.y);
    let x1 = ex.saturating_add(m).min(cx);
    let y1 = ey.saturating_add(m).min(cy);
    Ok(Rect {
        x: x0,
        y: y0,
        width: u32::try_from(x1.saturating_sub(x0).max(0)).map_err(|_| Error::Overflow)?,
        height: u32::try_from(y1.saturating_sub(y0).max(0)).map_err(|_| Error::Overflow)?,
    })
}

/// Bounding rectangle of the grid cells where `pred` holds (grid origin `o`, width `w`).
fn bbox(o: Rect, w: usize, pred: impl Fn(usize) -> bool, n: usize) -> Result<Option<Rect>, Error> {
    let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for i in 0..n {
        if pred(i) {
            let (x, y) = at(o, w, i);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 > x1 {
        return Ok(None);
    }
    let width = x1.checked_sub(x0).and_then(|v| v.checked_add(1)).ok_or(Error::Overflow)?;
    let height = y1.checked_sub(y0).and_then(|v| v.checked_add(1)).ok_or(Error::Overflow)?;
    Ok(Some(Rect {
        x: x0,
        y: y0,
        width: u32::try_from(width).map_err(|_| Error::Overflow)?,
        height: u32::try_from(height).map_err(|_| Error::Overflow)?,
    }))
}

fn plane_of(bounds: Rect, mask: &[bool]) -> CoveragePlane {
    CoveragePlane {
        bounds,
        data: mask.iter().map(|m| if *m { u16::MAX } else { 0 }).collect(),
    }
}

/// Segmented base-frame matte from the strokes (module docs). Output covers `frame.bounds`.
pub fn roto_base(frame: &RgbaPlane, strokes: &RotoStrokes<'_>, opts: &RotoOptions, limits: &RotoLimits) -> Result<CoveragePlane, ToolError> {
    check(opts)?;
    let (w, h) = check_frame(frame)?;
    let fb = frame.bounds;
    let n = w.checked_mul(h).ok_or(Error::Overflow)?;
    let out_bytes = fb.area().checked_mul(2).ok_or(Error::Overflow)?;
    if out_bytes > limits.max_output_bytes {
        return Err(Error::BudgetExceeded.into());
    }
    let painted = |p: Option<&CoveragePlane>, x: i64, y: i64| p.is_some_and(|p| p.at(x, y) >= 32768);
    let fg = |x: i64, y: i64| painted(Some(strokes.foreground), x, y);
    let bg = |x: i64, y: i64| painted(strokes.background, x, y);
    let Some(seed) = bbox(fb, w, |i| { let (x, y) = at(fb, w, i); fg(x, y) && !bg(x, y) }, n)? else {
        return Err(Error::InvalidInput.into());
    };
    let roi = grow(seed, opts.base_margin_px.ceil() as i64, fb)?;
    limits.tool.admit(roi.area(), WORK_BYTES_PER_PIXEL)?;
    let img = image(frame, roi, &limits.tool)?;
    let rw = roi.width as usize;
    let m = img.px.len();
    let mut hard = Vec::with_capacity(m);
    let mut bg_model = Vec::with_capacity(m);
    for i in 0..m {
        let (x, y) = at(roi, rw, i);
        let (dx, dy) = (x.saturating_sub(roi.x), y.saturating_sub(roi.y));
        let ring = dx < 2 || dy < 2 || dx >= i64::from(roi.width).saturating_sub(2) || dy >= i64::from(roi.height).saturating_sub(2);
        let state = if bg(x, y) {
            Hard::Bg
        } else if fg(x, y) {
            Hard::Fg
        } else {
            Hard::Free
        };
        bg_model.push(state == Hard::Bg || (state == Hard::Free && ring));
        hard.push(state);
    }
    let bins: Vec<u16> = img.px.iter().map(|p| bin(*p) as u16).collect();
    let fg0 = counts(&bins, |i| hard.get(i) == Some(&Hard::Fg));
    let bg0 = counts(&bins, |i| bg_model.get(i) == Some(&true));
    let labels = segment(&img, &hard, fg0, bg0, &opts.graph)?;
    let mut mask = vec![false; n];
    for (i, l) in labels.iter().enumerate() {
        let (x, y) = at(roi, rw, i);
        if *l && let Some(j) = index_in(fb, x, y) && let Some(s) = mask.get_mut(j) {
            *s = true;
        }
    }
    Ok(plane_of(fb, &mask))
}

/// One propagation step: the mask of `next` from the mask of `prev` (module docs).
fn step(
    prev: &RgbaPlane,
    prev_mask: &[bool],
    next: &RgbaPlane,
    opts: &RotoOptions,
    limits: &RotoLimits,
    cancel: &CancellationToken,
) -> Result<(Vec<bool>, u64), ToolError> {
    cancel.check().map_err(|_| Error::Canceled)?;
    let fb = next.bounds;
    let (w, h) = (fb.width as usize, fb.height as usize);
    let n = w.checked_mul(h).ok_or(Error::Overflow)?;
    if !prev_mask.iter().any(|m| *m) {
        return Ok((vec![false; n], 0));
    }
    let flow = flow_gray(&luma(next, w, h), &luma(prev, w, h), &opts.flow, cancel).map_err(blend_err)?;
    let soft = |x: f64, y: f64| bilinear(w, h, x, y, |i| if prev_mask.get(i) == Some(&true) { 1.0 } else { 0.0 });
    let pred: Vec<bool> = (0..n)
        .map(|i| {
            let (x, y) = (i.checked_rem(w).unwrap_or(0), i.checked_div(w).unwrap_or(0));
            let [u, v] = flow.at(x, y);
            soft(x as f64 + u, y as f64 + v) >= 0.5
        })
        .collect();
    if !pred.iter().any(|p| *p) {
        return Ok((pred, 0));
    }
    let to_out = edt_grid(&pred.iter().map(|p| !p).collect::<Vec<_>>(), w, h);
    let to_in = edt_grid(&pred, w, h);
    let hard: Vec<Hard> = (0..n)
        .map(|i| {
            let inside = pred.get(i) == Some(&true);
            let d2 = if inside { to_out.get(i) } else { to_in.get(i) };
            let d = d2.copied().unwrap_or(f64::INFINITY).sqrt() - 0.5;
            match (d > opts.band_px, inside) {
                (true, true) => Hard::Fg,
                (true, false) => Hard::Bg,
                (false, _) => Hard::Free,
            }
        })
        .collect();
    let Some(free) = bbox(fb, w, |i| hard.get(i) == Some(&Hard::Free), n)? else {
        return Ok((pred, 0));
    };
    let roi = grow(free, 2, fb)?;
    limits.tool.admit(roi.area(), WORK_BYTES_PER_PIXEL)?;
    let img = image(next, roi, &limits.tool)?;
    let prev_img = image(prev, roi, &limits.tool)?;
    let rw = roi.width as usize;
    let idx: Vec<usize> = (0..img.px.len())
        .map(|i| {
            let (x, y) = at(roi, rw, i);
            index_in(fb, x, y).ok_or(Error::Overflow)
        })
        .collect::<Result<_, _>>()?;
    let hard_roi: Vec<Hard> = idx.iter().map(|j| hard.get(*j).copied().unwrap_or(Hard::Free)).collect();
    let bins: Vec<u16> = img.px.iter().map(|p| bin(*p) as u16).collect();
    let prev_bins: Vec<u16> = prev_img.px.iter().map(|p| bin(*p) as u16).collect();
    let was = |i: usize| idx.get(i).and_then(|j| prev_mask.get(*j)).copied().unwrap_or(false);
    let mut fg0 = counts(&prev_bins, was);
    let mut bg0 = counts(&prev_bins, |i| !was(i));
    for (c, d) in fg0.iter_mut().zip(counts(&bins, |i| hard_roi.get(i) == Some(&Hard::Fg))) {
        *c = c.saturating_add(d);
    }
    for (c, d) in bg0.iter_mut().zip(counts(&bins, |i| hard_roi.get(i) == Some(&Hard::Bg))) {
        *c = c.saturating_add(d);
    }
    let labels = segment(&img, &hard_roi, fg0, bg0, &opts.graph)?;
    let mut mask: Vec<bool> = hard.iter().map(|s| *s == Hard::Fg).collect();
    let mut band = 0u64;
    for ((l, j), s) in labels.iter().zip(&idx).zip(&hard_roi) {
        if *s == Hard::Free {
            band = band.saturating_add(1);
        }
        if let Some(slot) = mask.get_mut(*j) {
            *slot = *l;
        }
    }
    Ok((mask, band))
}

/// Propagates `base_matte` (coverage `>= 0.5` = inside, read over the frame grid) from frame
/// `base_index` forward and backward through `frames` (module docs). Returns one [`RotoFrame`]
/// per frame, in order.
pub fn roto_propagate(
    frames: &[RgbaPlane],
    base_index: usize,
    base_matte: &CoveragePlane,
    opts: &RotoOptions,
    limits: &RotoLimits,
    cancel: &CancellationToken,
) -> Result<Vec<RotoFrame>, ToolError> {
    check(opts)?;
    let first = frames.first().ok_or(Error::InvalidInput)?;
    if frames.len() > limits.max_frames {
        return Err(Error::BudgetExceeded.into());
    }
    if base_index >= frames.len() {
        return Err(Error::InvalidInput.into());
    }
    let fb = first.bounds;
    // Aggregate admission before any work: all output mattes, per-step graph and flow bytes, and
    // the flow work of every step together.
    let count = u64::try_from(frames.len()).map_err(|_| Error::Overflow)?;
    let out = fb.area().checked_mul(2).and_then(|b| b.checked_mul(count)).ok_or(Error::Overflow)?;
    if out > limits.max_output_bytes {
        return Err(Error::BudgetExceeded.into());
    }
    limits.tool.admit(fb.area(), WORK_BYTES_PER_PIXEL)?;
    flow_admit(fb.area(), &opts.flow, count.saturating_sub(1), &limits.blend).map_err(blend_err)?;
    for f in frames {
        if f.bounds != fb {
            return Err(Error::InvalidInput.into());
        }
        check_frame(f)?;
    }
    let (w, h) = (fb.width as usize, fb.height as usize);
    let n = w.checked_mul(h).ok_or(Error::Overflow)?;
    let base: Vec<bool> = (0..n)
        .map(|i| {
            let (x, y) = at(fb, w, i);
            base_matte.at(x, y) >= 32768
        })
        .collect();
    if !base.iter().any(|b| *b) {
        return Err(Error::InvalidInput.into());
    }
    let mut masks: Vec<Option<(Vec<bool>, RotoStatus, u64)>> = vec![None; frames.len()];
    if let Some(slot) = masks.get_mut(base_index) {
        *slot = Some((base.clone(), RotoStatus::Base, 0));
    }
    let run = |order: &mut dyn Iterator<Item = (usize, usize)>, masks: &mut Vec<Option<(Vec<bool>, RotoStatus, u64)>>| -> Result<(), ToolError> {
        for (from, to) in order {
            let (prev_mask, prev_status) = match masks.get(from).and_then(|m| m.as_ref()) {
                Some((m, s, _)) => (m.clone(), *s),
                None => return Err(Error::InvalidInput.into()),
            };
            let (prev, next) = (frames.get(from).ok_or(Error::InvalidInput)?, frames.get(to).ok_or(Error::InvalidInput)?);
            let (mask, band) = if prev_status == RotoStatus::Lost {
                (vec![false; n], 0)
            } else {
                step(prev, &prev_mask, next, opts, limits, cancel)?
            };
            let status = if mask.iter().any(|m| *m) { RotoStatus::Propagated } else { RotoStatus::Lost };
            if let Some(slot) = masks.get_mut(to) {
                *slot = Some((mask, status, band));
            }
        }
        Ok(())
    };
    let last = frames.len().saturating_sub(1);
    run(&mut (base_index..last).map(|k| (k, k.saturating_add(1))), &mut masks)?;
    run(&mut (1..=base_index).rev().map(|k| (k, k.saturating_sub(1))), &mut masks)?;
    masks
        .into_iter()
        .enumerate()
        .map(|(index, m)| {
            let (mask, status, band_pixels) = m.ok_or(Error::InvalidInput)?;
            Ok(RotoFrame {
                index,
                matte: plane_of(fb, &mask),
                status,
                band_pixels,
            })
        })
        .collect()
}

/// Admission of a whole assisted-roto run before any allocation (lane SMB-AE-ROTO-WIRING;
/// STU-CMP-034 item 8: "Frame count, per-frame working set, aggregate output bytes and aggregate
/// motion-estimation work are admitted against declared limits BEFORE any allocation"). `frames`
/// = every frame of every span, `propagation_steps` = the flow steps of propagation, `refine_passes`
/// = refine-stage passes over all frames (each: 22 bytes per pixel per frame, see
/// [`super::roto_refine`]), `chatter_steps` = chatter flow estimates. Refusals are typed
/// (`BudgetExceeded`, `Overflow`, `tool_out_of_range` for the options).
pub fn roto_sequence_admission(
    frame: Rect,
    frames: u64,
    propagation_steps: u64,
    refine_passes: u64,
    chatter_steps: u64,
    opts: &RotoOptions,
    limits: &RotoLimits,
) -> Result<(), ToolError> {
    check(opts)?;
    if usize::try_from(frames).map_err(|_| Error::Overflow)? > limits.max_frames {
        return Err(Error::BudgetExceeded.into());
    }
    let area = frame.area();
    let mattes = area.checked_mul(2).and_then(|b| b.checked_mul(frames)).ok_or(Error::Overflow)?;
    let refine = area
        .checked_mul(22)
        .and_then(|b| b.checked_mul(frames))
        .and_then(|b| b.checked_mul(refine_passes))
        .ok_or(Error::Overflow)?;
    if mattes.checked_add(refine).ok_or(Error::Overflow)? > limits.max_output_bytes {
        return Err(Error::BudgetExceeded.into());
    }
    limits.tool.admit(area, WORK_BYTES_PER_PIXEL)?;
    let steps = propagation_steps.checked_add(chatter_steps).ok_or(Error::Overflow)?;
    flow_admit(area, &opts.flow, steps, &limits.blend).map_err(blend_err)?;
    Ok(())
}
