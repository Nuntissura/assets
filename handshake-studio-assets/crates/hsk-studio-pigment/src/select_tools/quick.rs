//! Quick selection brush and rectangle GrabCut by graph-cut segmentation (STU-RAS-018 rows
//! "Quick/selection brush | Painted selection that grows to matching regions and snaps to edges,
//! with add/subtract by stroke" and "Object select", STU-RAS-019, STU-RAS-122 quick-select brush).
//!
//! Clean-room from the published papers (no code read):
//! - Boykov and Jolly, "Interactive Graph Cuts for Optimal Boundary & Region Segmentation of
//!   Objects in N-D Images", ICCV 2001: hard seed constraints as infinite t-links, a regional term
//!   `-ln Pr(I_p | label)` from seed intensity histograms, a boundary term on neighbour pairs, and
//!   the global optimum by a minimum s-t cut.
//! - Rother, Kolmogorov and Blake, "GrabCut", SIGGRAPH 2004: n-link weight
//!   `gamma * exp(-beta |I_p - I_q|^2)` with `beta = 1 / (2 <|I_p - I_q|^2>)` and `gamma = 50`,
//!   rectangle initialisation, and iterated re-estimation of the colour models from the current
//!   segmentation.
//!
//! Studio readings (class Approximate, Photoshop UNVERIFIED, D5): colour models are additively
//! smoothed (0.01 per bin) 16x16x16 histograms of premultiplied RGB (the BJ2001 histogram form,
//! not GrabCut's GMMs); 4-connected n-links; capacities are `round(1024 * weight)` integers; a
//! stroke's ROI is its footprint grown by `roi_margin_px` (default `3 * diameter + 8`); the
//! other side's initial model is the ROI's 2-pixel border ring plus a band of half the margin
//! around the ROI (unselected pixels for Add, selected pixels for Subtract).
//!
//! Object / subject / sky select need an on-device ML model (STU-RAS-019); none is installed and
//! nothing is downloaded, so they refuse `model_unavailable`.
#![deny(clippy::arithmetic_side_effects)]
use super::maxflow::{BYTES_PER_ARC, BYTES_PER_NODE, FlowGraph};
use super::{Composite, RgbaImage, ToolError, ToolLimits, admit_plane, binary_plane, coords_of, tighten};
use crate::brush::{BrushDynamics, BrushLimits, BrushTip, StrokeSample, place_dabs, stroke_coverage};
use crate::selection::{ColourView, CombineMode, CoveragePlane, SelectionContext, combine};
use crate::{Error, Rect};

/// Whether a stroke adds to or subtracts from the selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StrokeMode {
    #[default]
    Add,
    Subtract,
}

/// One quick-selection stroke: pointer samples, the brush diameter and the mode. Dabs are placed
/// by the one brush engine ([`crate::brush::place_dabs`], hard round tip, 25 % spacing).
#[derive(Clone, Copy, Debug)]
pub struct QuickStroke<'a> {
    pub samples: &'a [StrokeSample],
    pub diameter_px: f64,
    pub mode: StrokeMode,
}

/// Graph-cut options. Nothing is clamped: out-of-domain values refuse.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GraphCutOptions {
    /// Boundary weight (GrabCut gamma); finite, `>= 0`.
    pub gamma: f64,
    /// Colour-model re-estimation rounds (at least one cut always runs).
    pub iterations: u32,
    /// ROI growth around the stroke footprint or rectangle; `None` = tool default.
    pub roi_margin_px: Option<f64>,
    pub sample_merged: bool,
    /// Arc-scan budget of the max-flow solver (hostile graphs refuse `budget_exceeded`).
    pub max_flow_steps: u64,
}
impl Default for GraphCutOptions {
    fn default() -> Self {
        Self {
            gamma: 50.0,
            iterations: 3,
            roi_margin_px: None,
            sample_merged: false,
            max_flow_steps: 1 << 34,
        }
    }
}

/// Working bytes per ROI pixel: the premultiplied image copy (32) + graph node (12) + 8 arcs
/// (4 edges: right, down, source, sink) + hard label, two model masks and the label (4) + the
/// two n-link weights (16).
pub const WORK_BYTES_PER_PIXEL: u64 = RgbaImage::BYTES_PER_PIXEL + BYTES_PER_NODE + 8 * BYTES_PER_ARC + 4 + 16;

const SCALE: f64 = 1024.0;
const BINS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hard {
    Free,
    Fg,
    Bg,
}

fn bin(p: [f64; 4]) -> usize {
    let q = |v: f64| ((v.clamp(0.0, 1.0) * 16.0) as usize).min(15);
    (q(p[0]) << 8) | (q(p[1]) << 4) | q(p[2])
}

/// Pseudo-count added to every histogram bin (additive smoothing). Small, so a seed set of a few
/// hundred pixels is not drowned by the 4096 empty bins (Studio reading).
const SMOOTHING: f64 = 0.01;

/// Histogram of the pixels where `member` holds.
fn counts(bins: &[u16], member: impl Fn(usize) -> bool) -> Vec<u64> {
    let mut c = vec![0u64; BINS];
    for (i, b) in bins.iter().enumerate() {
        if member(i)
            && let Some(slot) = c.get_mut(usize::from(*b))
        {
            *slot = slot.saturating_add(1);
        }
    }
    c
}

/// `-ln` of the additively smoothed histogram.
fn model(counts: &[u64]) -> Vec<f64> {
    let total: u64 = counts.iter().fold(0u64, |a, c| a.saturating_add(*c));
    let denom = total as f64 + SMOOTHING * BINS as f64;
    counts.iter().map(|c| -((*c as f64 + SMOOTHING) / denom).ln()).collect()
}

/// Adds the pixels of `outer` outside `roi` where `member(x, y)` holds to `into` (the colour of
/// what surrounds the ROI). Reads only; `outer` was admitted by the caller.
fn add_band(
    into: &mut [u64],
    src: &Composite<'_>,
    outer: Rect,
    roi: Rect,
    member: impl Fn(i64, i64) -> bool,
) -> Result<(), Error> {
    let (ex, ey) = outer.end()?;
    for y in outer.y..ey {
        for x in outer.x..ex {
            if super::index_in(roi, x, y).is_some() || !member(x, y) {
                continue;
            }
            if let Some(slot) = into.get_mut(bin(src.premul(x, y)?)) {
                *slot = slot.saturating_add(1);
            }
        }
    }
    Ok(())
}

fn check_options(opts: &GraphCutOptions) -> Result<(), ToolError> {
    if !opts.gamma.is_finite() || opts.roi_margin_px.is_some_and(|m| !m.is_finite()) {
        return Err(Error::Nonfinite.into());
    }
    if opts.gamma < 0.0 || opts.roi_margin_px.is_some_and(|m| m < 0.0) {
        return Err(ToolError::OutOfRange);
    }
    Ok(())
}

fn margin_px(m: f64) -> Result<i64, Error> {
    let c = m.ceil();
    if c > 1.0e12 {
        return Err(Error::Overflow);
    }
    Ok(c as i64)
}

/// `r` grown by `m` on every side, clipped to `canvas` (`None` when disjoint).
fn grow_clip(r: Rect, m: i64, canvas: Rect) -> Result<Option<Rect>, Error> {
    let (ex, ey) = r.end()?;
    let x0 = r.x.saturating_sub(m).max(canvas.x);
    let y0 = r.y.saturating_sub(m).max(canvas.y);
    let (cx, cy) = canvas.end()?;
    let x1 = ex.saturating_add(m).min(cx);
    let y1 = ey.saturating_add(m).min(cy);
    if x1 <= x0 || y1 <= y0 {
        return Ok(None);
    }
    Ok(Some(Rect {
        x: x0,
        y: y0,
        width: u32::try_from(x1.checked_sub(x0).ok_or(Error::Overflow)?).map_err(|_| Error::Overflow)?,
        height: u32::try_from(y1.checked_sub(y0).ok_or(Error::Overflow)?).map_err(|_| Error::Overflow)?,
    }))
}

/// Minimum-cut segmentation of `img`: hard labels, initial model masks, re-estimated models.
/// Returns the foreground flag per pixel.
fn segment(
    img: &RgbaImage,
    hard: &[Hard],
    fg0: Vec<u64>,
    bg0: Vec<u64>,
    opts: &GraphCutOptions,
) -> Result<Vec<bool>, ToolError> {
    let b = img.bounds;
    let n = img.px.len();
    let w = b.width as usize;
    let bins: Vec<u16> = img.px.iter().map(|p| bin(*p) as u16).collect();
    let d2 = |i: usize, j: usize| -> f64 {
        let (p, q) = (img.px.get(i).copied().unwrap_or([0.0; 4]), img.px.get(j).copied().unwrap_or([0.0; 4]));
        (0..4).map(|c| (p[c] - q[c]) * (p[c] - q[c])).sum()
    };
    // beta = 1 / (2 <|dI|^2>) over 4-neighbour pairs (GrabCut); 0 when the image is flat.
    let (mut sum, mut pairs) = (0.0f64, 0u64);
    for i in 0..n {
        let x = i.checked_rem(w.max(1)).unwrap_or(0);
        if x.saturating_add(1) < w {
            sum += d2(i, i.saturating_add(1));
            pairs = pairs.saturating_add(1);
        }
        let below = i.saturating_add(w);
        if below < n {
            sum += d2(i, below);
            pairs = pairs.saturating_add(1);
        }
    }
    let mean = if pairs > 0 { sum / pairs as f64 } else { 0.0 };
    let beta = if mean > 0.0 { 1.0 / (2.0 * mean) } else { 0.0 };
    let weight = |i: usize, j: usize| -> i64 { (opts.gamma * (-beta * d2(i, j)).exp() * SCALE).round() as i64 };
    let mut right = vec![0i64; n];
    let mut down = vec![0i64; n];
    let mut nsum = 0i64;
    for i in 0..n {
        let x = i.checked_rem(w.max(1)).unwrap_or(0);
        if x.saturating_add(1) < w
            && let Some(r) = right.get_mut(i)
        {
            *r = weight(i, i.saturating_add(1));
            nsum = nsum.saturating_add(*r);
        }
        let below = i.saturating_add(w);
        if below < n
            && let Some(d) = down.get_mut(i)
        {
            *d = weight(i, below);
            nsum = nsum.saturating_add(*d);
        }
    }
    // A hard t-link costs more than every n-link together (BJ2001 K).
    let infinite = nsum.saturating_add(1).max(1 << 40);
    let (s, t) = (n, n.saturating_add(1));
    let mut labels: Vec<bool> = hard.iter().map(|h| *h == Hard::Fg).collect();
    let rounds = opts.iterations.max(1);
    for round in 0..rounds {
        let (fg_cost, bg_cost) = if round == 0 {
            (model(&fg0), model(&bg0))
        } else {
            (
                model(&counts(&bins, |i| labels.get(i) == Some(&true))),
                model(&counts(&bins, |i| labels.get(i) == Some(&false))),
            )
        };
        let edges = n.checked_mul(4).ok_or(Error::Overflow)?;
        let mut g = FlowGraph::with_capacity(n.checked_add(2).ok_or(Error::Overflow)?, edges)?;
        for i in 0..n {
            let bi = usize::from(bins.get(i).copied().unwrap_or(0));
            // cap(s, p) = cost of labelling p background; cap(p, t) = cost of foreground.
            let (mut cs, mut ct) = match hard.get(i).copied().unwrap_or(Hard::Free) {
                Hard::Fg => (infinite, 0),
                Hard::Bg => (0, infinite),
                Hard::Free => (
                    (bg_cost.get(bi).copied().unwrap_or(0.0) * SCALE).round() as i64,
                    (fg_cost.get(bi).copied().unwrap_or(0.0) * SCALE).round() as i64,
                ),
            };
            // Both t-links of a pixel share their minimum: subtracting it changes every cut by
            // the same constant, so the minimum cut is unchanged.
            let m = cs.min(ct);
            cs = cs.saturating_sub(m);
            ct = ct.saturating_sub(m);
            if cs > 0 {
                g.add_edge(s, i, cs, 0)?;
            }
            if ct > 0 {
                g.add_edge(i, t, ct, 0)?;
            }
            let r = right.get(i).copied().unwrap_or(0);
            if r > 0 {
                g.add_edge(i, i.saturating_add(1), r, r)?;
            }
            let d = down.get(i).copied().unwrap_or(0);
            if d > 0 {
                g.add_edge(i, i.saturating_add(w), d, d)?;
            }
        }
        g.max_flow(s, t, opts.max_flow_steps)?;
        let side = g.source_side(s);
        let next: Vec<bool> = side.into_iter().take(n).collect();
        let stable = next == labels && round > 0;
        labels = next;
        if stable {
            break;
        }
    }
    Ok(labels)
}

fn ring(b: Rect, i: usize, width: u32) -> bool {
    let (x, y) = coords_of(b, i);
    let (dx, dy) = (x.saturating_sub(b.x), y.saturating_sub(b.y));
    let w = i64::from(width);
    dx < w
        || dy < w
        || dx >= i64::from(b.width).saturating_sub(w)
        || dy >= i64::from(b.height).saturating_sub(w)
}

fn empty(canvas: Rect) -> CoveragePlane {
    CoveragePlane {
        bounds: Rect {
            x: canvas.x,
            y: canvas.y,
            width: 0,
            height: 0,
        },
        data: Vec::new(),
    }
}

/// One quick-selection stroke against the current selection `base`: Add grows the selection to
/// the region the stroke indicates (never removes), Subtract removes it (never adds). The
/// result is `base` outside the stroke's ROI.
pub fn quick_select(
    layers: &[ColourView<'_>],
    active: usize,
    base: Option<&CoveragePlane>,
    stroke: &QuickStroke<'_>,
    opts: &GraphCutOptions,
    ctx: &SelectionContext,
    limits: &ToolLimits,
) -> Result<CoveragePlane, ToolError> {
    check_options(opts)?;
    let tip = BrushTip::round(stroke.diameter_px, 100.0, 25.0);
    let blim = BrushLimits {
        max_bytes: ctx.limits.max_bytes,
        ..BrushLimits::default()
    };
    let dabs = place_dabs(&tip, &BrushDynamics::default(), stroke.samples, &blim)?;
    let keep = || base.cloned().unwrap_or_else(|| empty(ctx.canvas));
    if dabs.is_empty() {
        return Ok(keep());
    }
    let foot = stroke_coverage(&dabs, 100.0, ctx.canvas, &blim)?;
    if foot.bounds.area() == 0 {
        return Ok(keep());
    }
    let margin = margin_px(opts.roi_margin_px.unwrap_or(stroke.diameter_px * 3.0 + 8.0))?;
    let Some(roi) = grow_clip(foot.bounds, margin, ctx.canvas)? else {
        return Ok(keep());
    };
    admit_plane(roi, &ctx.limits)?;
    limits.admit(roi.area(), WORK_BYTES_PER_PIXEL)?;
    let src = Composite::select(layers, active, opts.sample_merged)?;
    let img = RgbaImage::from_composite(&src, roi, limits)?;
    let n = img.px.len();
    let (mut hard, mut fg0, mut bg0) = (vec![Hard::Free; n], vec![false; n], vec![false; n]);
    for i in 0..n {
        let (x, y) = coords_of(roi, i);
        let seed = foot.at(x, y) >= 32768;
        let in_base = base.is_some_and(|b| b.at(x, y) >= 32768);
        let (h, f, g) = match stroke.mode {
            StrokeMode::Add => (
                if seed || in_base { Hard::Fg } else { Hard::Free },
                seed,
                !seed && !in_base && ring(roi, i, 2),
            ),
            StrokeMode::Subtract => (
                if seed || !in_base { Hard::Bg } else { Hard::Free },
                in_base && !seed && ring(roi, i, 2),
                seed,
            ),
        };
        if let (Some(hs), Some(fs), Some(gs)) = (hard.get_mut(i), fg0.get_mut(i), bg0.get_mut(i)) {
            *hs = h;
            *fs = f;
            *gs = g;
        }
    }
    // Initial colour models: the stroke for its own side; for the other side, the ROI's border
    // ring plus a band of half the margin around the ROI (what surrounds the stroke and is, for
    // Add, not selected / for Subtract, selected).
    let bins: Vec<u16> = img.px.iter().map(|p| bin(*p) as u16).collect();
    let mut fg_counts = counts(&bins, |i| fg0.get(i) == Some(&true));
    let mut bg_counts = counts(&bins, |i| bg0.get(i) == Some(&true));
    if let Some(outer) = grow_clip(roi, margin.checked_div(2).unwrap_or(0), ctx.canvas)? {
        limits.admit(outer.area(), RgbaImage::BYTES_PER_PIXEL)?;
        let in_base = |x: i64, y: i64| base.is_some_and(|b| b.at(x, y) >= 32768);
        match stroke.mode {
            StrokeMode::Add => add_band(&mut bg_counts, &src, outer, roi, |x, y| !in_base(x, y))?,
            StrokeMode::Subtract => add_band(&mut fg_counts, &src, outer, roi, in_base)?,
        }
    }
    let labels = segment(&img, &hard, fg_counts, bg_counts, opts)?;
    let out = match stroke.mode {
        StrokeMode::Add => {
            let seg = binary_plane(roi, |i| labels.get(i) == Some(&true), ctx.anti_alias)?;
            combine(base, &seg, CombineMode::Extend, &ctx.limits)?
        }
        StrokeMode::Subtract => {
            let Some(b) = base else {
                return Ok(empty(ctx.canvas));
            };
            let removed = binary_plane(roi, |i| labels.get(i) == Some(&false), ctx.anti_alias)?;
            combine(Some(b), &removed, CombineMode::Diminish, &ctx.limits)?
        }
    };
    Ok(tighten(out)?)
}

/// Rectangle-initialised GrabCut: pixels outside `rect` are hard background, pixels inside start
/// as foreground and are re-labelled by iterated minimum cuts. A non-ML stand-in for the
/// rectangle form of object select; the ROI is `rect` grown by `roi_margin_px` (default 8).
pub fn grabcut_rect(
    layers: &[ColourView<'_>],
    active: usize,
    rect: Rect,
    opts: &GraphCutOptions,
    ctx: &SelectionContext,
    limits: &ToolLimits,
) -> Result<CoveragePlane, ToolError> {
    check_options(opts)?;
    let inner = rect.intersect(ctx.canvas)?.ok_or(Error::InvalidInput)?;
    let margin = margin_px(opts.roi_margin_px.unwrap_or(8.0))?;
    let roi = grow_clip(inner, margin, ctx.canvas)?.ok_or(Error::InvalidInput)?;
    admit_plane(roi, &ctx.limits)?;
    limits.admit(roi.area(), WORK_BYTES_PER_PIXEL)?;
    let src = Composite::select(layers, active, opts.sample_merged)?;
    let img = RgbaImage::from_composite(&src, roi, limits)?;
    let n = img.px.len();
    let inside: Vec<bool> = (0..n)
        .map(|i| {
            let (x, y) = coords_of(roi, i);
            super::index_in(inner, x, y).is_some()
        })
        .collect();
    let hard: Vec<Hard> = inside.iter().map(|v| if *v { Hard::Free } else { Hard::Bg }).collect();
    let bins: Vec<u16> = img.px.iter().map(|p| bin(*p) as u16).collect();
    let fg0 = counts(&bins, |i| inside.get(i) == Some(&true));
    let bg0 = counts(&bins, |i| inside.get(i) == Some(&false));
    let labels = segment(&img, &hard, fg0, bg0, opts)?;
    let seg = binary_plane(roi, |i| labels.get(i) == Some(&true), ctx.anti_alias)?;
    Ok(tighten(seg)?)
}

/// Region an object-select request names (hover point, rectangle or lasso).
#[derive(Clone, Debug, PartialEq)]
pub enum ObjectRegion {
    Hover { x: i64, y: i64 },
    Rect(Rect),
    Lasso(Vec<[f64; 2]>),
}

/// Object select (STU-RAS-018/019) needs an on-device segmentation model. No model is installed
/// and nothing is downloaded: always refuses `model_unavailable`. [`grabcut_rect`] is the non-ML
/// rectangle alternative.
pub fn object_select(
    _layers: &[ColourView<'_>],
    _region: &ObjectRegion,
    _ctx: &SelectionContext,
) -> Result<CoveragePlane, ToolError> {
    Err(ToolError::ModelUnavailable)
}

/// Subject select (STU-RAS-018/019): needs an on-device model; refuses `model_unavailable`.
pub fn subject_select(_layers: &[ColourView<'_>], _ctx: &SelectionContext) -> Result<CoveragePlane, ToolError> {
    Err(ToolError::ModelUnavailable)
}

/// Sky select (STU-RAS-018/019): needs an on-device model; refuses `model_unavailable`.
pub fn sky_select(_layers: &[ColourView<'_>], _ctx: &SelectionContext) -> Result<CoveragePlane, ToolError> {
    Err(ToolError::ModelUnavailable)
}
