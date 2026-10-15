//! Point tracking and track solves (lane SMB-AE-KEY-TRACK slice 2, After Effects parity gap AE-19).
//!
//! Spec basis (module 14 section 14.27.4): STU-CMP-065 (point tracking: track points with a feature
//! region, a search region and an attach point, analysed forward or backward over a frame range,
//! producing per-frame positions; one point solves position, two solve position, rotation and
//! scale, four solve a corner pin; a track is correctable by re-analysing from any frame),
//! STU-CMP-065a (long-running, cancellable, observable: determinate progress, typed command
//! surface, no window), STU-CMP-065b (declared gap: feature/search sizing, the confidence metric,
//! its threshold and the solve's error metric are authored here, see below). Single owner
//! (CX-CAP-001) of the tracking pixel analysis; the keyframes it feeds are written by composite.
//!
//! Coordinates: positions are layer pixels where pixel `(i, j)` covers `[i, i+1) x [j, j+1)`, so its
//! sample sits at its centre `(i + 0.5, j + 0.5)` (the composite layer-space convention).
//!
//! Tracker: the pyramidal Lucas-Kanade feature tracker as described by J.-Y. Bouguet, "Pyramidal
//! Implementation of the Lucas Kanade Feature Tracker" (Intel, 2000), after B. D. Lucas and
//! T. Kanade (IJCAI 1981) and C. Tomasi and T. Kanade (CMU-CS-91-132, 1991); implemented from the
//! papers. Per frame pair (the previous frame `I`, the next `J`; frame to frame, so the feature
//! adapts every frame):
//! - Pyramid: level `L + 1` averages the 2x2 blocks of level `L` with the separable `[1 3 3 1] / 8`
//!   filter (edges clamped), so a level-`L` coordinate is the level-0 coordinate divided by `2^L`.
//!   The level count is the smallest `L` with `ceil(r / 2) (2^(L+1) - 1) >= s` (`r` the feature
//!   radius, `s` the search radius: each level is trusted with half a window of motion), at most
//!   [`MAX_PYRAMID_LEVELS`] and while the coarsest level keeps `2 r + 3` pixels per side.
//! - Per level, coarsest first, with the guess `g` from the coarser level (`0` at the top): the
//!   gradient matrix `G = sum [Ix^2, IxIy; IxIy, Iy^2]` over the `(2r + 1)^2` window of `I`
//!   (central differences of bilinear samples), then up to `max_iterations` steps
//!   `eta = G^-1 sum (I(x) - J(x + g + v)) [Ix, Iy]`, `v += eta`, stopping when `|eta| <
//!   epsilon_px`; `g` for the next finer level is `2 (g + v)`. At level 0 the translation is then
//!   refined with the six-parameter affine window model of J. Shi and C. Tomasi, "Good Features to
//!   Track" (CVPR 1994), so a window that rotates or scales between frames does not bias the
//!   position (kept only when the system is regular and the deformation stays below 0.5).
//! - Status (authored, STU-CMP-065b): `Degenerate` when the per-sample minimum eigenvalue of `G` at
//!   level 0 is below `min_eigen` (flat or one-directional texture: the aperture problem) or the
//!   feature window has no variance; `Lost` when the frame-to-frame motion leaves the search region
//!   (`|dx| > s` or `|dy| > s`) or is non-finite; `OutOfFrame` when the new feature region (the
//!   position plus or minus `r`) leaves the frame; otherwise `Tracked` or `LowConfidence` by the confidence below. A point that is not
//!   tracked keeps its last position with confidence 0 for the remaining frames.
//! - Confidence: the zero-mean normalised cross-correlation of the level-0 feature window of `I`
//!   at the old position and of `J` at the new one, floored at 0 (1 = identical texture).
//!
//! Solves (relative to a reference frame): one point = offset `p_t - p_ref`; two points (`a`, `b`)
//! = offset of `a`, rotation `atan2` of `b - a` against the reference (degrees, clockwise on screen
//! since y points down, unwrapped to the previous frame) and scale `|b_t - a_t| / |b_ref - a_ref|`;
//! four points = the homography mapping the reference corners to frame `t`'s (normalised DLT,
//! Hartley 1997, solved by Gaussian elimination with partial pivoting; a near-singular system is
//! `Degenerate`).
//!
//! Determinism: binary64 IEEE `+ - * /` and `sqrt` in a fixed order; `atan2` (two-point rotation)
//! is the platform libm's, so its last-ulp agreement across hosts is UNVERIFIED. No GPU path.
//! After Effects' tracker internals are UNVERIFIED (D5); Studio's readings above are authored.
//!
//! Hostile inputs (V4-01/02): parameters are range-checked against [`TrackLimits`] (never clamped);
//! frames are checked (one size for all, finite, `[0, 1]`) and the pyramid bytes and the tracking
//! work are computed with checked arithmetic and refused before any allocation.
#![deny(clippy::arithmetic_side_effects)]
use crate::retouch::RgbaPlane;
use hsk_studio_accord::CancellationToken;
use serde::Serialize;
use std::fmt;

/// Largest pyramid depth (levels below level 0).
pub const MAX_PYRAMID_LEVELS: u32 = 6;

/// Typed failure of the tracking maths.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackError {
    /// A parameter is non-finite or outside its range (including search regions above the limit).
    Parameter,
    /// A buffer length disagrees with its size, frames differ in size, or track lengths differ.
    Shape,
    /// A sample outside `[0, 1]` or non-finite.
    OutOfDomain,
    /// Bytes or work above the caller's limits (refused before allocation).
    OverBudget { needed: u64, limit: u64 },
    Overflow,
    Canceled,
    /// A solve on coincident or collinear points.
    Degenerate,
}
impl TrackError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Parameter => "track_parameter",
            Self::Shape => "track_shape",
            Self::OutOfDomain => "track_out_of_domain",
            Self::OverBudget { .. } => "track_over_budget",
            Self::Overflow => "track_overflow",
            Self::Canceled => "track_canceled",
            Self::Degenerate => "track_degenerate",
        }
    }
}
impl fmt::Display for TrackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for TrackError {}

/// Bounds of one tracking run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackLimits {
    pub max_frame_pixels: u64,
    pub max_frames: usize,
    pub max_points: usize,
    pub max_feature_radius: u32,
    pub max_search_radius: u32,
    pub max_bytes: u64,
    pub max_work: u64,
}
impl TrackLimits {
    pub const DEFAULT: Self = Self {
        max_frame_pixels: 1 << 26,
        max_frames: 100_000,
        max_points: 64,
        max_feature_radius: 64,
        max_search_radius: 1024,
        max_bytes: 1 << 30,
        max_work: 20_000_000_000,
    };
}

/// One luminance frame, row-major, values in `[0, 1]`.
#[derive(Clone, Debug, PartialEq)]
pub struct LumaFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>,
}
impl LumaFrame {
    /// Checked frame (length, finite, `[0, 1]`).
    pub fn new(width: u32, height: u32, data: Vec<f32>) -> Result<Self, TrackError> {
        let f = Self { width, height, data };
        f.validate()?;
        Ok(f)
    }
    pub fn validate(&self) -> Result<(), TrackError> {
        let n = u64::from(self.width).checked_mul(u64::from(self.height)).ok_or(TrackError::Overflow)?;
        if usize::try_from(n).map_err(|_| TrackError::Overflow)? != self.data.len() || n == 0 {
            return Err(TrackError::Shape);
        }
        if self.data.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
            return Err(TrackError::OutOfDomain);
        }
        Ok(())
    }
    /// BT.709 luma of the premultiplied colour of a plane.
    pub fn from_plane(p: &RgbaPlane) -> Result<Self, TrackError> {
        let data = p
            .data
            .iter()
            .map(|s| {
                let a = f64::from(s[3]);
                (a * (0.2126 * f64::from(s[0]) + 0.7152 * f64::from(s[1]) + 0.0722 * f64::from(s[2]))) as f32
            })
            .collect();
        Self::new(p.bounds.width, p.bounds.height, data)
    }
}

/// One track point to analyse.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct TrackPointSpec {
    /// Feature position at the start frame (layer pixels).
    pub position: [f64; 2],
    /// Half-size of the square feature region, `1..=max_feature_radius`.
    pub feature_radius: u32,
    /// Largest frame-to-frame motion per axis, `1..=max_search_radius`.
    pub search_radius: u32,
}

/// Solver parameters (authored, STU-CMP-065b).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct TrackParams {
    /// `1..=100`.
    pub max_iterations: u32,
    /// Step size below which a level stops, `(0, 1]`.
    pub epsilon_px: f64,
    /// Minimum per-sample eigenvalue of the gradient matrix, `0..=1`.
    pub min_eigen: f64,
    /// Confidence below which a frame is `LowConfidence`, `0..=1`.
    pub min_confidence: f64,
}
impl TrackParams {
    pub const DEFAULT: Self = Self {
        max_iterations: 30,
        epsilon_px: 0.001,
        min_eigen: 1e-6,
        min_confidence: 0.8,
    };
    fn validate(&self) -> Result<(), TrackError> {
        let ok = (1..=100).contains(&self.max_iterations)
            && self.epsilon_px.is_finite()
            && self.epsilon_px > 0.0
            && self.epsilon_px <= 1.0
            && self.min_eigen.is_finite()
            && (0.0..=1.0).contains(&self.min_eigen)
            && self.min_confidence.is_finite()
            && (0.0..=1.0).contains(&self.min_confidence);
        if ok { Ok(()) } else { Err(TrackError::Parameter) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum TrackDirection {
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum TrackStatus {
    Tracked,
    LowConfidence,
    Lost,
    OutOfFrame,
    Degenerate,
}
impl TrackStatus {
    /// Whether the position of this frame came from analysis.
    pub const fn is_tracked(self) -> bool {
        matches!(self, Self::Tracked | Self::LowConfidence)
    }
}

/// One analysed frame of one point.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct TrackSample {
    pub frame: usize,
    pub position: [f64; 2],
    pub confidence: f64,
    pub status: TrackStatus,
}

struct Level {
    w: usize,
    h: usize,
    data: Vec<f32>,
}
impl Level {
    fn at(&self, x: usize, y: usize) -> f64 {
        f64::from(self.data[y.saturating_mul(self.w).saturating_add(x)])
    }
    /// Bilinear sample at a continuous coordinate (pixel centres at `+0.5`), edges clamped.
    fn sample(&self, x: f64, y: f64) -> f64 {
        let max_x = self.w.saturating_sub(1) as f64;
        let max_y = self.h.saturating_sub(1) as f64;
        let u = (x - 0.5).clamp(0.0, max_x);
        let v = (y - 0.5).clamp(0.0, max_y);
        let (fu, fv) = (u.floor(), v.floor());
        // In range by the clamp above.
        let (i0, j0) = (fu as usize, fv as usize);
        let i1 = i0.saturating_add(1).min(self.w.saturating_sub(1));
        let j1 = j0.saturating_add(1).min(self.h.saturating_sub(1));
        let (tx, ty) = (u - fu, v - fv);
        let top = self.at(i0, j0) * (1.0 - tx) + self.at(i1, j0) * tx;
        let bottom = self.at(i0, j1) * (1.0 - tx) + self.at(i1, j1) * tx;
        top * (1.0 - ty) + bottom * ty
    }
    fn grad(&self, x: f64, y: f64) -> (f64, f64) {
        (
            (self.sample(x + 1.0, y) - self.sample(x - 1.0, y)) / 2.0,
            (self.sample(x, y + 1.0) - self.sample(x, y - 1.0)) / 2.0,
        )
    }
    /// Half resolution with the `[1 3 3 1] / 8` filter (edges clamped).
    fn down(&self) -> Self {
        let w = self.w.div_ceil(2);
        let h = self.h.div_ceil(2);
        let k = [1.0 / 8.0, 3.0 / 8.0, 3.0 / 8.0, 1.0 / 8.0];
        let clampi = |v: usize, off: usize, n: usize| v.saturating_add(off).saturating_sub(1).min(n.saturating_sub(1));
        let mut tmp = vec![0.0f64; w.saturating_mul(self.h)];
        for y in 0..self.h {
            for x in 0..w {
                let mut acc = 0.0;
                for (t, kv) in k.iter().enumerate() {
                    acc += kv * self.at(clampi(x.saturating_mul(2), t, self.w), y);
                }
                tmp[y.saturating_mul(w).saturating_add(x)] = acc;
            }
        }
        let mut data = vec![0.0f32; w.saturating_mul(h)];
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for (t, kv) in k.iter().enumerate() {
                    acc += kv * tmp[clampi(y.saturating_mul(2), t, self.h).saturating_mul(w).saturating_add(x)];
                }
                data[y.saturating_mul(w).saturating_add(x)] = acc as f32;
            }
        }
        Self { w, h, data }
    }
}

fn pyramid(frame: &LumaFrame, levels: u32) -> Vec<Level> {
    let mut out = vec![Level {
        w: frame.width as usize,
        h: frame.height as usize,
        data: frame.data.clone(),
    }];
    for _ in 0..levels {
        if let Some(last) = out.last() {
            let next = last.down();
            out.push(next);
        }
    }
    out
}

fn pow2(l: u32) -> f64 {
    let mut v = 1.0;
    for _ in 0..l.min(30) {
        v *= 2.0;
    }
    v
}

fn above(v: f64, t: f64) -> bool {
    v > t
}

/// Pyramid depth for a feature radius `r` and search radius `s` on a `w x h` frame.
pub fn pyramid_levels(r: u32, s: u32, w: u32, h: u32) -> u32 {
    let mut l = 0u32;
    let need = u64::from(r).saturating_mul(2).saturating_add(3);
    while l < MAX_PYRAMID_LEVELS {
        let reach = u64::from(r.div_ceil(2)).saturating_mul(1u64.checked_shl(l.saturating_add(1)).unwrap_or(u64::MAX).saturating_sub(1));
        if reach >= u64::from(s) {
            break;
        }
        let next = l.saturating_add(1);
        if u64::from(w).checked_shr(next).unwrap_or(0) < need || u64::from(h).checked_shr(next).unwrap_or(0) < need {
            break;
        }
        l = next;
    }
    l
}

/// Predicted peak bytes and work of [`track_points`].
pub fn track_cost(
    frame_pixels: u64,
    frames: u64,
    points: &[TrackPointSpec],
    levels: u32,
    params: &TrackParams,
) -> Result<(u64, u64), TrackError> {
    let ov = TrackError::Overflow;
    // Two pyramids (< 4/3 each, counted as 2) of f32 plus one f64 scratch level.
    let bytes = frame_pixels.checked_mul(16 + 8).ok_or(ov)?;
    let samples = u64::try_from(points.len()).map_err(|_| ov)?.checked_mul(frames).ok_or(ov)?.checked_mul(48).ok_or(ov)?;
    let bytes = bytes.checked_add(samples).ok_or(ov)?;
    let mut work = frame_pixels.checked_mul(frames).ok_or(ov)?.checked_mul(8).ok_or(ov)?;
    for p in points {
        let side = u64::from(p.feature_radius).checked_mul(2).and_then(|v| v.checked_add(1)).ok_or(ov)?;
        let per = side
            .checked_mul(side)
            .and_then(|v| v.checked_mul(u64::from(params.max_iterations).checked_add(4)?))
            .and_then(|v| v.checked_mul(u64::from(levels).checked_add(1)?))
            .and_then(|v| v.checked_mul(frames))
            .and_then(|v| v.checked_mul(8))
            .ok_or(ov)?;
        work = work.checked_add(per).ok_or(ov)?;
    }
    Ok((bytes, work))
}

/// Progress of a run: frames done of the total (determinate, STU-CMP-065a).
pub type TrackProgress<'a> = &'a mut dyn FnMut(u64, u64);

/// Tracks `points` from frame `start` to the last frame (`Forward`) or to frame 0 (`Backward`),
/// frame to frame. Returns, per point, one sample per frame of the range in frame order (the start
/// frame is the given position, `Tracked`, confidence 1, unless its texture is degenerate).
/// Re-running from any frame with a corrected position is the manual-correction path.
#[allow(clippy::too_many_arguments)]
pub fn track_points(
    frames: &[LumaFrame],
    start: usize,
    direction: TrackDirection,
    points: &[TrackPointSpec],
    params: &TrackParams,
    limits: &TrackLimits,
    cancel: &CancellationToken,
    progress: TrackProgress<'_>,
) -> Result<Vec<Vec<TrackSample>>, TrackError> {
    params.validate()?;
    if frames.is_empty() || frames.len() > limits.max_frames || start >= frames.len() {
        return Err(TrackError::Parameter);
    }
    if points.is_empty() || points.len() > limits.max_points {
        return Err(TrackError::Parameter);
    }
    let first = &frames[0];
    for f in frames {
        if f.width != first.width || f.height != first.height {
            return Err(TrackError::Shape);
        }
    }
    let n = u64::from(first.width).checked_mul(u64::from(first.height)).ok_or(TrackError::Overflow)?;
    if n > limits.max_frame_pixels {
        return Err(TrackError::OverBudget { needed: n, limit: limits.max_frame_pixels });
    }
    for f in frames {
        f.validate()?;
    }
    let (fw, fh) = (f64::from(first.width), f64::from(first.height));
    let mut max_r = 0u32;
    let mut max_s = 0u32;
    for p in points {
        let pos_ok = p.position.iter().all(|v| v.is_finite()) && (0.0..fw).contains(&p.position[0]) && (0.0..fh).contains(&p.position[1]);
        if !pos_ok
            || p.feature_radius == 0
            || p.feature_radius > limits.max_feature_radius
            || p.search_radius == 0
            || p.search_radius > limits.max_search_radius
        {
            return Err(TrackError::Parameter);
        }
        max_r = max_r.max(p.feature_radius);
        max_s = max_s.max(p.search_radius);
    }
    let levels = pyramid_levels(max_r, max_s, first.width, first.height);
    let span = match direction {
        TrackDirection::Forward => frames.len().saturating_sub(start),
        TrackDirection::Backward => start.saturating_add(1),
    };
    let span_u = u64::try_from(span).map_err(|_| TrackError::Overflow)?;
    let (bytes, work) = track_cost(n, span_u, points, levels, params)?;
    if bytes > limits.max_bytes {
        return Err(TrackError::OverBudget { needed: bytes, limit: limits.max_bytes });
    }
    if work > limits.max_work {
        return Err(TrackError::OverBudget { needed: work, limit: limits.max_work });
    }

    let order: Vec<usize> = match direction {
        TrackDirection::Forward => (start..frames.len()).collect(),
        TrackDirection::Backward => (0..=start).rev().collect(),
    };
    let mut out: Vec<Vec<TrackSample>> = points.iter().map(|_| Vec::with_capacity(span)).collect();
    let mut prev = pyramid(&frames[start], levels);
    let mut state: Vec<(bool, [f64; 2])> = Vec::with_capacity(points.len());
    for (p, o) in points.iter().zip(out.iter_mut()) {
        let ok = feature_quality(&prev[0], p.position, p.feature_radius, params.min_eigen);
        state.push((ok, p.position));
        o.push(TrackSample {
            frame: start,
            position: p.position,
            confidence: if ok { 1.0 } else { 0.0 },
            status: if ok { TrackStatus::Tracked } else { TrackStatus::Degenerate },
        });
    }
    progress(1, span_u);
    for (done, &fi) in order.iter().enumerate().skip(1) {
        cancel.check().map_err(|_| TrackError::Canceled)?;
        let next = pyramid(&frames[fi], levels);
        for ((p, o), st) in points.iter().zip(out.iter_mut()).zip(state.iter_mut()) {
            let sample = if st.0 {
                let s = track_one(&prev, &next, st.1, p, params);
                if s.status.is_tracked() {
                    st.1 = s.position;
                } else {
                    st.0 = false;
                }
                TrackSample { frame: fi, ..s }
            } else {
                let status = o.last().map_or(TrackStatus::Lost, |l| if l.status.is_tracked() { TrackStatus::Lost } else { l.status });
                TrackSample { frame: fi, position: st.1, confidence: 0.0, status }
            };
            o.push(sample);
        }
        prev = next;
        progress(u64::try_from(done).map_err(|_| TrackError::Overflow)?.saturating_add(1), span_u);
    }
    for o in &mut out {
        o.sort_by_key(|s| s.frame);
    }
    Ok(out)
}

fn window(r: u32) -> std::ops::RangeInclusive<i64> {
    let r = i64::from(r);
    r.saturating_neg()..=r
}

/// Per-sample minimum eigenvalue of the gradient matrix and window variance both above threshold.
fn feature_quality(l: &Level, p: [f64; 2], r: u32, min_eigen: f64) -> bool {
    let (mut a, mut b, mut c) = (0.0, 0.0, 0.0);
    let (mut s, mut s2, mut k) = (0.0, 0.0, 0.0);
    for dy in window(r) {
        for dx in window(r) {
            let (x, y) = (p[0] + dx as f64, p[1] + dy as f64);
            let (gx, gy) = l.grad(x, y);
            a += gx * gx;
            b += gx * gy;
            c += gy * gy;
            let v = l.sample(x, y);
            s += v;
            s2 += v * v;
            k += 1.0;
        }
    }
    let lmin = (a + c) / 2.0 - (((a - c) / 2.0) * ((a - c) / 2.0) + b * b).sqrt();
    let var = s2 / k - (s / k) * (s / k);
    lmin / k >= min_eigen && var > 1e-12
}

fn ncc(i: &Level, j: &Level, p: [f64; 2], q: [f64; 2], r: u32) -> f64 {
    let mut ti = Vec::new();
    let mut tj = Vec::new();
    for dy in window(r) {
        for dx in window(r) {
            ti.push(i.sample(p[0] + dx as f64, p[1] + dy as f64));
            tj.push(j.sample(q[0] + dx as f64, q[1] + dy as f64));
        }
    }
    let k = ti.len() as f64;
    let mi = ti.iter().sum::<f64>() / k;
    let mj = tj.iter().sum::<f64>() / k;
    let (mut num, mut di, mut dj) = (0.0, 0.0, 0.0);
    for (a, b) in ti.iter().zip(&tj) {
        num += (a - mi) * (b - mj);
        di += (a - mi) * (a - mi);
        dj += (b - mj) * (b - mj);
    }
    let den = (di * dj).sqrt();
    if den <= 0.0 { 0.0 } else { (num / den).max(0.0) }
}

fn track_one(prev: &[Level], next: &[Level], p: [f64; 2], spec: &TrackPointSpec, params: &TrackParams) -> TrackSample {
    let r = spec.feature_radius;
    let mut g = [0.0f64; 2];
    let top = prev.len().saturating_sub(1);
    for l in (0..=top).rev() {
        let (il, jl) = (&prev[l], &next[l]);
        let scale = pow2(u32::try_from(l).unwrap_or(0));
        let u = [p[0] / scale, p[1] / scale];
        let (mut a, mut b, mut c) = (0.0, 0.0, 0.0);
        let mut pts = Vec::new();
        for dy in window(r) {
            for dx in window(r) {
                let (x, y) = (u[0] + dx as f64, u[1] + dy as f64);
                let (gx, gy) = il.grad(x, y);
                a += gx * gx;
                b += gx * gy;
                c += gy * gy;
                pts.push((x, y, il.sample(x, y), gx, gy));
            }
        }
        let det = a * c - b * b;
        let mut v = [0.0f64; 2];
        if det.is_finite() && det.abs() > 1e-18 {
            for _ in 0..params.max_iterations {
                let (mut bx, mut by) = (0.0, 0.0);
                for &(x, y, iv, gx, gy) in &pts {
                    let e = iv - jl.sample(x + g[0] + v[0], y + g[1] + v[1]);
                    bx += e * gx;
                    by += e * gy;
                }
                let ex = (c * bx - b * by) / det;
                let ey = (a * by - b * bx) / det;
                v[0] += ex;
                v[1] += ey;
                if !(v[0].is_finite() && v[1].is_finite()) {
                    break;
                }
                if (ex * ex + ey * ey).sqrt() < params.epsilon_px {
                    break;
                }
            }
        }
        g = if l == 0 {
            let t = [g[0] + v[0], g[1] + v[1]];
            affine_refine(jl, u, t, &pts, params).unwrap_or(t)
        } else {
            [2.0 * (g[0] + v[0]), 2.0 * (g[1] + v[1])]
        };
    }
    let s = f64::from(spec.search_radius);
    let q = [p[0] + g[0], p[1] + g[1]];
    let lost = |status| TrackSample { frame: 0, position: p, confidence: 0.0, status };
    if !(g[0].is_finite() && g[1].is_finite()) {
        return lost(TrackStatus::Lost);
    }
    // The feature region must stay inside the frame (edge-clamped samples carry no information).
    let (w, h, rf) = (prev[0].w as f64, prev[0].h as f64, f64::from(r));
    if q[0] - rf < 0.0 || q[1] - rf < 0.0 || q[0] + rf > w || q[1] + rf > h {
        return lost(TrackStatus::OutOfFrame);
    }
    if g[0].abs() > s || g[1].abs() > s {
        return lost(TrackStatus::Lost);
    }
    let confidence = ncc(&prev[0], &next[0], p, q, r);
    let status = if confidence >= params.min_confidence { TrackStatus::Tracked } else { TrackStatus::LowConfidence };
    TrackSample { frame: 0, position: q, confidence, status }
}

/// Shi-Tomasi affine refinement at level 0 (J. Shi and C. Tomasi, "Good Features to Track",
/// CVPR 1994): the window of `J` is warped by `x -> x + d + A (x - u)` and the six parameters are
/// refined by Gauss-Newton steps against the template gradients of `I`, starting from the
/// translation `d` of the pure-translation stage. `None` (keep the translation) when the system is
/// singular, the deformation grows above 0.5, or the result is non-finite.
fn affine_refine(
    jl: &Level,
    u: [f64; 2],
    d: [f64; 2],
    pts: &[(f64, f64, f64, f64, f64)],
    params: &TrackParams,
) -> Option<[f64; 2]> {
    let mut g6 = [[0.0f64; 6]; 6];
    let rows: Vec<[f64; 6]> = pts
        .iter()
        .map(|&(x, y, _, gx, gy)| {
            let (rx, ry) = (x - u[0], y - u[1]);
            [gx, gy, gx * rx, gx * ry, gy * rx, gy * ry]
        })
        .collect();
    for row in &rows {
        for (gi, ri) in g6.iter_mut().zip(row) {
            for (gij, rj) in gi.iter_mut().zip(row) {
                *gij += ri * rj;
            }
        }
    }
    let mut p = [d[0], d[1], 0.0, 0.0, 0.0, 0.0];
    for _ in 0..params.max_iterations {
        let mut b = [0.0f64; 6];
        for (&(x, y, iv, _, _), row) in pts.iter().zip(&rows) {
            let (rx, ry) = (x - u[0], y - u[1]);
            let wx = x + p[0] + p[2] * rx + p[3] * ry;
            let wy = y + p[1] + p[4] * rx + p[5] * ry;
            let e = iv - jl.sample(wx, wy);
            for (bi, ri) in b.iter_mut().zip(row) {
                *bi += ri * e;
            }
        }
        let step = solve_n(g6, b).ok()?;
        for (pi, si) in p.iter_mut().zip(step) {
            *pi += si;
        }
        if p.iter().any(|v| !v.is_finite()) || p[2..].iter().any(|v| v.abs() > 0.5) {
            return None;
        }
        if (step[0] * step[0] + step[1] * step[1]).sqrt() < params.epsilon_px
            && step[2..].iter().all(|v| v.abs() < params.epsilon_px * 1e-2)
        {
            break;
        }
    }
    Some([p[0], p[1]])
}

// ---- solves ----

fn sample_at(track: &[TrackSample], frame: usize) -> Option<&TrackSample> {
    track.binary_search_by_key(&frame, |s| s.frame).ok().and_then(|i| track.get(i))
}

fn aligned(tracks: &[&[TrackSample]]) -> Result<(), TrackError> {
    let Some(first) = tracks.first() else {
        return Err(TrackError::Shape);
    };
    for t in tracks {
        if t.len() != first.len() || t.iter().zip(first.iter()).any(|(a, b)| a.frame != b.frame) {
            return Err(TrackError::Shape);
        }
    }
    Ok(())
}

/// One-point solve: per frame `(frame, offset p_t - p_ref)`.
pub fn solve_one_point(track: &[TrackSample], reference: usize) -> Result<Vec<(usize, [f64; 2])>, TrackError> {
    let r = sample_at(track, reference).ok_or(TrackError::Parameter)?.position;
    Ok(track.iter().map(|s| (s.frame, [s.position[0] - r[0], s.position[1] - r[1]])).collect())
}

/// Two-point solve result for one frame, relative to the reference frame, about point `a`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct SimilarityFrame {
    pub frame: usize,
    /// `a_t - a_ref`.
    pub offset: [f64; 2],
    /// Degrees, clockwise on screen, unwrapped along the track.
    pub rotation_deg: f64,
    /// Ratio of `|b - a|` to the reference.
    pub scale: f64,
}

/// Two-point solve (position, rotation and scale).
pub fn solve_two_point(a: &[TrackSample], b: &[TrackSample], reference: usize) -> Result<Vec<SimilarityFrame>, TrackError> {
    aligned(&[a, b])?;
    let ar = sample_at(a, reference).ok_or(TrackError::Parameter)?.position;
    let br = sample_at(b, reference).ok_or(TrackError::Parameter)?.position;
    let v0 = [br[0] - ar[0], br[1] - ar[1]];
    let len0 = (v0[0] * v0[0] + v0[1] * v0[1]).sqrt();
    if !above(len0, 1e-6) {
        return Err(TrackError::Degenerate);
    }
    let ang0 = v0[1].atan2(v0[0]);
    let mut out = Vec::with_capacity(a.len());
    // Unwrap from the reference outward in both directions.
    let ri = a.iter().position(|s| s.frame == reference).ok_or(TrackError::Parameter)?;
    let solve = |i: usize, prev: f64| -> Result<SimilarityFrame, TrackError> {
        let (pa, pb) = (a[i].position, b[i].position);
        let v = [pb[0] - pa[0], pb[1] - pa[1]];
        let len = (v[0] * v[0] + v[1] * v[1]).sqrt();
        if !above(len, 1e-6) {
            return Err(TrackError::Degenerate);
        }
        let mut deg = (v[1].atan2(v[0]) - ang0).to_degrees();
        while deg - prev > 180.0 {
            deg -= 360.0;
        }
        while deg - prev < -180.0 {
            deg += 360.0;
        }
        Ok(SimilarityFrame {
            frame: a[i].frame,
            offset: [pa[0] - ar[0], pa[1] - ar[1]],
            rotation_deg: deg,
            scale: len / len0,
        })
    };
    let mut fwd = Vec::new();
    let mut prev = 0.0;
    for i in ri..a.len() {
        let s = solve(i, prev)?;
        prev = s.rotation_deg;
        fwd.push(s);
    }
    let mut back = Vec::new();
    prev = 0.0;
    for i in (0..ri).rev() {
        let s = solve(i, prev)?;
        prev = s.rotation_deg;
        back.push(s);
    }
    back.reverse();
    out.extend(back);
    out.extend(fwd);
    Ok(out)
}

/// A projective transform `x' = (m0 x + m1 y + m2) / (m6 x + m7 y + m8)`,
/// `y' = (m3 x + m4 y + m5) / (m6 x + m7 y + m8)`, normalised to `m8 = 1`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Homography {
    pub m: [f64; 9],
}
impl Homography {
    pub const IDENTITY: Self = Self { m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] };
    pub fn apply(&self, p: [f64; 2]) -> [f64; 2] {
        let m = &self.m;
        let w = m[6] * p[0] + m[7] * p[1] + m[8];
        [(m[0] * p[0] + m[1] * p[1] + m[2]) / w, (m[3] * p[0] + m[4] * p[1] + m[5]) / w]
    }
    /// Least-squares homography mapping `src[i]` to `dst[i]` (4 or more pairs; exact for 4),
    /// normalised DLT with `h33 = 1`. `Degenerate` on collinear or coincident points.
    #[allow(clippy::needless_range_loop)]
    pub fn fit(src: &[[f64; 2]], dst: &[[f64; 2]]) -> Result<Self, TrackError> {
        if src.len() != dst.len() || src.len() < 4 {
            return Err(TrackError::Shape);
        }
        if src.iter().chain(dst.iter()).any(|p| !(p[0].is_finite() && p[1].is_finite())) {
            return Err(TrackError::Parameter);
        }
        let (ts, ns) = normaliser(src)?;
        let (td, nd) = normaliser(dst)?;
        // Normal equations A^T A h = A^T b, 8 unknowns.
        let mut ata = [[0.0f64; 8]; 8];
        let mut atb = [0.0f64; 8];
        for (s, d) in src.iter().zip(dst) {
            let (x, y) = ((s[0] - ts[0]) * ns, (s[1] - ts[1]) * ns);
            let (u, v) = ((d[0] - td[0]) * nd, (d[1] - td[1]) * nd);
            let rows = [
                ([x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y], u),
                ([0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y], v),
            ];
            for (row, rhs) in rows {
                for i in 0..8 {
                    atb[i] += row[i] * rhs;
                    for j in 0..8 {
                        ata[i][j] += row[i] * row[j];
                    }
                }
            }
        }
        let h = solve_n(ata, atb)?;
        // Denormalise: H = Td^-1 Hn Ts.
        let hn = [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0];
        let tsm = [ns, 0.0, -ns * ts[0], 0.0, ns, -ns * ts[1], 0.0, 0.0, 1.0];
        let tdi = [1.0 / nd, 0.0, td[0], 0.0, 1.0 / nd, td[1], 0.0, 0.0, 1.0];
        let m = mul3(&tdi, &mul3(&hn, &tsm));
        if !above(m[8].abs(), 1e-15) || m.iter().any(|v| !v.is_finite()) {
            return Err(TrackError::Degenerate);
        }
        let mut out = [0.0; 9];
        for (o, v) in out.iter_mut().zip(m) {
            *o = v / m[8];
        }
        Ok(Self { m: out })
    }
}

fn normaliser(p: &[[f64; 2]]) -> Result<([f64; 2], f64), TrackError> {
    let k = p.len() as f64;
    let c = [p.iter().map(|q| q[0]).sum::<f64>() / k, p.iter().map(|q| q[1]).sum::<f64>() / k];
    let d = p.iter().map(|q| ((q[0] - c[0]) * (q[0] - c[0]) + (q[1] - c[1]) * (q[1] - c[1])).sqrt()).sum::<f64>() / k;
    if !above(d, 1e-9) {
        return Err(TrackError::Degenerate);
    }
    Ok((c, std::f64::consts::SQRT_2 / d))
}

fn mul3(a: &[f64; 9], b: &[f64; 9]) -> [f64; 9] {
    let row = |m: &[f64; 9], r: usize| -> [f64; 3] {
        match r {
            0 => [m[0], m[1], m[2]],
            1 => [m[3], m[4], m[5]],
            _ => [m[6], m[7], m[8]],
        }
    };
    let col = |m: &[f64; 9], c: usize| -> [f64; 3] {
        match c {
            0 => [m[0], m[3], m[6]],
            1 => [m[1], m[4], m[7]],
            _ => [m[2], m[5], m[8]],
        }
    };
    let dot = |x: [f64; 3], y: [f64; 3]| x[0] * y[0] + x[1] * y[1] + x[2] * y[2];
    let mut out = [0.0; 9];
    let mut it = out.iter_mut();
    for r in 0..3 {
        for c in 0..3 {
            if let Some(o) = it.next() {
                *o = dot(row(a, r), col(b, c));
            }
        }
    }
    out
}

#[allow(clippy::needless_range_loop)]
/// Gaussian elimination with partial pivoting; `Degenerate` below a relative pivot of 1e-12.
fn solve_n<const N: usize>(mut a: [[f64; N]; N], mut b: [f64; N]) -> Result<[f64; N], TrackError> {
    let scale = a.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()));
    if !above(scale, 0.0) || !scale.is_finite() {
        return Err(TrackError::Degenerate);
    }
    for col in 0..N {
        let mut piv = col;
        for r in col..N {
            if a[r][col].abs() > a[piv][col].abs() {
                piv = r;
            }
        }
        if !above(a[piv][col].abs(), 1e-12 * scale) {
            return Err(TrackError::Degenerate);
        }
        a.swap(col, piv);
        b.swap(col, piv);
        for r in 0..N {
            if r == col {
                continue;
            }
            let f = a[r][col] / a[col][col];
            if f == 0.0 {
                continue;
            }
            for c in col..N {
                a[r][c] -= f * a[col][c];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = [0.0; N];
    for i in 0..N {
        x[i] = b[i] / a[i][i];
    }
    Ok(x)
}

/// Four-point (corner pin) solve: per frame the homography from the reference corners to the
/// frame's corners.
#[allow(clippy::needless_range_loop)]
pub fn solve_corner_pin(corners: [&[TrackSample]; 4], reference: usize) -> Result<Vec<(usize, Homography)>, TrackError> {
    aligned(&corners)?;
    let mut src = [[0.0; 2]; 4];
    for (s, t) in src.iter_mut().zip(corners) {
        *s = sample_at(t, reference).ok_or(TrackError::Parameter)?.position;
    }
    let mut out = Vec::with_capacity(corners[0].len());
    for i in 0..corners[0].len() {
        let dst = [corners[0][i].position, corners[1][i].position, corners[2][i].position, corners[3][i].position];
        out.push((corners[0][i].frame, Homography::fit(&src, &dst)?));
    }
    Ok(out)
}
