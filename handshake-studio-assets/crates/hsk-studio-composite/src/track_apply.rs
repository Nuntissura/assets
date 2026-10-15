//! Apply track data to a composition layer and stabilise (lane SMB-AE-KEY-TRACK slice 3, After
//! Effects parity gap AE-19).
//!
//! Spec basis: STU-CMP-065 ("a track's output is ordinary keyframes on ordinary properties
//! ([STU-MOT-030])"; "Apply to target: applying a track writes keyframes onto a chosen target's
//! chosen properties, with an explicit choice of which properties receive them. It is a normal
//! reversible history step and the written keyframes are ordinary keyframes with no hidden link";
//! "Stabilization: the same analysis applied inversely to hold a feature still, with an explicit
//! choice of which axes to stabilize and a smoothing amount for smooth-motion rather than lock-off
//! stabilization"). The pixel analysis is pigment's ([`hsk_studio_pigment::track_math`]); this
//! module only turns its result into Folio `StudioPropertyStream` keyframes, the records the
//! composition adapter already plays (`temporal::composition`, After Effects layer model: layer
//! point `x` lands at `position + R(rotation) S(scale) (x - anchor)`). Both functions are pure:
//! they return a NEW layer record and leave the input untouched, so the caller's history step is
//! the swap of the two records (reversible by swapping back).
//!
//! Model: a [`MotionTrack`] is, per analysed frame, the attach point (the tracked point `a`, in
//! the tracked layer's space, which for footage placed at the composition origin is composition
//! pixels), a rotation (degrees, clockwise on screen) and a scale ratio, both relative to the
//! reference frame (one-point tracks carry rotation 0 and scale 1). Only frames whose pigment
//! status is tracked produce a keyframe; frame `f` sits at tick `first_tick + f * ticks_per_frame`.
//! Every keyframe is linear in and out (the After Effects tracker's key interpolation is
//! UNVERIFIED, D5).
//!
//! - [`apply_track`]: position `= attach_t + attach_offset`, rotation `= base + rotation_t`, scale
//!   `= base * scale_t` (both axes), each only when chosen; an existing stream of a chosen property
//!   is replaced, its static value kept.
//! - [`stabilize`]: the anchor follows the feature (`anchor_t = attach_t`, layer space) so the
//!   transform pivots on it; with the position axis chosen `position_t = target_t`, else
//!   `position_t = attach_t` (no translation correction); rotation `= target_rot_t - rotation_t`,
//!   scale `= 100 * target_scale_t / scale_t`, each only when chosen. `target` is the reference
//!   value for lock-off (`smoothing_frames = 0`), else the centred moving average of the track over
//!   `2 k + 1` analysed frames (clipped at the ends), so slow camera motion is kept and shake
//!   removed. The feature point then lands on `target_t` on every frame (test
//!   `tests/track_apply.rs`).
//!
//! Hostile inputs (V4-01/02): non-finite or non-positive values, empty tracks, unsorted frames,
//! more than [`MAX_TRACK_KEYS`] keys, smoothing above [`MAX_SMOOTHING_FRAMES`] and tick overflow
//! are refused typed before anything is built.
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_folio::{
    StudioCompositionKeyframe, StudioCompositionLayer, StudioInterpolation, StudioLayerProperty,
    StudioPropertyStream,
};
use hsk_studio_pigment::track_math::{SimilarityFrame, TrackSample};
use std::fmt;

/// Most keys one stream may receive (motion's `MAX_KEYFRAMES`).
pub const MAX_TRACK_KEYS: usize = 65_536;
/// Largest smoothing radius in frames.
pub const MAX_SMOOTHING_FRAMES: u32 = 1_000;

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackApplyError {
    /// Non-finite, non-positive or out-of-range value, empty or unsorted track.
    Parameter,
    /// More keys than [`MAX_TRACK_KEYS`].
    KeyLimit,
    /// A tick leaves `u64`.
    TickOverflow,
}
impl TrackApplyError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Parameter => "track_apply_parameter",
            Self::KeyLimit => "track_apply_key_limit",
            Self::TickOverflow => "track_apply_tick_overflow",
        }
    }
}
impl fmt::Display for TrackApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for TrackApplyError {}

/// One analysed frame of a motion track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackFrame {
    pub frame: usize,
    pub attach: [f64; 2],
    pub rotation_deg: f64,
    pub scale: f64,
}

/// A motion track: analysed frames in increasing frame order.
#[derive(Clone, Debug, PartialEq)]
pub struct MotionTrack {
    frames: Vec<TrackFrame>,
}
impl MotionTrack {
    /// Checked track (non-empty, at most [`MAX_TRACK_KEYS`], strictly increasing frames, finite
    /// values, positive scale).
    pub fn new(frames: Vec<TrackFrame>) -> Result<Self, TrackApplyError> {
        if frames.is_empty() {
            return Err(TrackApplyError::Parameter);
        }
        if frames.len() > MAX_TRACK_KEYS {
            return Err(TrackApplyError::KeyLimit);
        }
        for w in frames.windows(2) {
            if w[0].frame >= w[1].frame {
                return Err(TrackApplyError::Parameter);
            }
        }
        let ok = frames.iter().all(|f| {
            f.attach.iter().all(|v| v.is_finite()) && f.rotation_deg.is_finite() && f.scale.is_finite() && f.scale > 0.0
        });
        if !ok {
            return Err(TrackApplyError::Parameter);
        }
        Ok(Self { frames })
    }
    /// One-point track: the tracked frames of `samples` (rotation 0, scale 1).
    pub fn from_one_point(samples: &[TrackSample]) -> Result<Self, TrackApplyError> {
        Self::new(
            samples
                .iter()
                .filter(|s| s.status.is_tracked())
                .map(|s| TrackFrame { frame: s.frame, attach: s.position, rotation_deg: 0.0, scale: 1.0 })
                .collect(),
        )
    }
    /// Two-point track: point `a`'s and point `b`'s samples and their two-point solve (same
    /// frames); only frames where both points are tracked.
    pub fn from_two_point(a: &[TrackSample], b: &[TrackSample], solve: &[SimilarityFrame]) -> Result<Self, TrackApplyError> {
        if a.len() != solve.len() || b.len() != solve.len() {
            return Err(TrackApplyError::Parameter);
        }
        let mut frames = Vec::with_capacity(a.len());
        for ((sa, sb), s) in a.iter().zip(b).zip(solve) {
            if sa.frame != s.frame || sb.frame != s.frame {
                return Err(TrackApplyError::Parameter);
            }
            if sa.status.is_tracked() && sb.status.is_tracked() {
                frames.push(TrackFrame { frame: s.frame, attach: sa.position, rotation_deg: s.rotation_deg, scale: s.scale });
            }
        }
        Self::new(frames)
    }
    pub fn frames(&self) -> &[TrackFrame] {
        &self.frames
    }
}

/// Frame `f` sits at composition tick `first_tick + f * ticks_per_frame`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackTiming {
    pub first_tick: u64,
    /// `>= 1`.
    pub ticks_per_frame: u64,
}
impl TrackTiming {
    fn tick(&self, frame: usize) -> Result<u64, TrackApplyError> {
        if self.ticks_per_frame == 0 {
            return Err(TrackApplyError::Parameter);
        }
        let f = u64::try_from(frame).map_err(|_| TrackApplyError::TickOverflow)?;
        f.checked_mul(self.ticks_per_frame)
            .and_then(|v| v.checked_add(self.first_tick))
            .ok_or(TrackApplyError::TickOverflow)
    }
}

/// Which properties receive keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TrackAxes {
    pub position: bool,
    pub rotation: bool,
    pub scale: bool,
}

fn key(time: u64, value: Vec<f64>) -> StudioCompositionKeyframe {
    StudioCompositionKeyframe {
        time,
        value,
        in_interp: StudioInterpolation::Linear,
        out_interp: StudioInterpolation::Linear,
        in_tangent: Vec::new(),
        out_tangent: Vec::new(),
    }
}

/// Replaces (or adds) the stream of `property`, keeping an existing static value.
fn put(layer: &mut StudioCompositionLayer, property: StudioLayerProperty, keys: Vec<StudioCompositionKeyframe>) {
    let fallback = keys.first().map(|k| k.value.clone()).unwrap_or_default();
    if let Some(s) = layer.streams.iter_mut().find(|s| s.property == property) {
        s.keyframes = keys;
    } else {
        layer.streams.push(StudioPropertyStream { property, static_value: fallback, keyframes: keys });
        layer.streams.sort_by_key(|s| s.property);
    }
}

fn finite(v: &[f64]) -> Result<(), TrackApplyError> {
    if v.iter().all(|x| x.is_finite()) { Ok(()) } else { Err(TrackApplyError::Parameter) }
}

/// Writes the track onto `target`'s chosen properties (see the module docs). `base_rotation_deg`
/// and `base_scale_pct` are the target's values the track's relative rotation and scale act on.
pub fn apply_track(
    target: &StudioCompositionLayer,
    track: &MotionTrack,
    timing: TrackTiming,
    axes: TrackAxes,
    attach_offset: [f64; 2],
    base_rotation_deg: f64,
    base_scale_pct: [f64; 2],
) -> Result<StudioCompositionLayer, TrackApplyError> {
    finite(&attach_offset)?;
    finite(&[base_rotation_deg, base_scale_pct[0], base_scale_pct[1]])?;
    if !(axes.position || axes.rotation || axes.scale) {
        return Err(TrackApplyError::Parameter);
    }
    let mut pos = Vec::new();
    let mut rot = Vec::new();
    let mut scl = Vec::new();
    for f in track.frames() {
        let t = timing.tick(f.frame)?;
        let p = vec![f.attach[0] + attach_offset[0], f.attach[1] + attach_offset[1]];
        let r = vec![base_rotation_deg + f.rotation_deg];
        let s = vec![base_scale_pct[0] * f.scale, base_scale_pct[1] * f.scale];
        finite(&p)?;
        finite(&r)?;
        finite(&s)?;
        pos.push(key(t, p));
        rot.push(key(t, r));
        scl.push(key(t, s));
    }
    let mut out = target.clone();
    if axes.position {
        put(&mut out, StudioLayerProperty::Position, pos);
    }
    if axes.rotation {
        put(&mut out, StudioLayerProperty::Rotation, rot);
    }
    if axes.scale {
        put(&mut out, StudioLayerProperty::Scale, scl);
    }
    Ok(out)
}

/// Centred moving average of `v` over `2 k + 1` entries, clipped at the ends.
fn smooth(v: &[f64], k: usize) -> Vec<f64> {
    let n = v.len();
    (0..n)
        .map(|i| {
            let lo = i.saturating_sub(k);
            let hi = i.saturating_add(k).min(n.saturating_sub(1));
            let window = v.get(lo..=hi).unwrap_or(&[]);
            window.iter().sum::<f64>() / window.len() as f64
        })
        .collect()
}

/// Stabilises the tracked layer itself (see the module docs). `reference` is the frame whose
/// placement is held for lock-off; `smoothing_frames` 0 is lock-off.
pub fn stabilize(
    layer: &StudioCompositionLayer,
    track: &MotionTrack,
    timing: TrackTiming,
    axes: TrackAxes,
    reference: usize,
    smoothing_frames: u32,
) -> Result<StudioCompositionLayer, TrackApplyError> {
    if smoothing_frames > MAX_SMOOTHING_FRAMES || !(axes.position || axes.rotation || axes.scale) {
        return Err(TrackApplyError::Parameter);
    }
    let fr = track.frames();
    let r = fr.iter().find(|f| f.frame == reference).ok_or(TrackApplyError::Parameter)?;
    let k = usize::try_from(smoothing_frames).map_err(|_| TrackApplyError::Parameter)?;
    let (tx, ty, tr, ts) = if k == 0 {
        let n = fr.len();
        (vec![r.attach[0]; n], vec![r.attach[1]; n], vec![r.rotation_deg; n], vec![r.scale; n])
    } else {
        let col = |g: fn(&TrackFrame) -> f64| -> Vec<f64> { smooth(&fr.iter().map(g).collect::<Vec<_>>(), k) };
        (col(|f| f.attach[0]), col(|f| f.attach[1]), col(|f| f.rotation_deg), col(|f| f.scale))
    };
    let mut anchor = Vec::with_capacity(fr.len());
    let mut pos = Vec::with_capacity(fr.len());
    let mut rot = Vec::with_capacity(fr.len());
    let mut scl = Vec::with_capacity(fr.len());
    for (i, f) in fr.iter().enumerate() {
        let t = timing.tick(f.frame)?;
        anchor.push(key(t, f.attach.to_vec()));
        let p = if axes.position { vec![tx[i], ty[i]] } else { f.attach.to_vec() };
        let rv = vec![tr[i] - f.rotation_deg];
        let s = 100.0 * ts[i] / f.scale;
        let sv = vec![s, s];
        finite(&p)?;
        finite(&rv)?;
        finite(&sv)?;
        pos.push(key(t, p));
        rot.push(key(t, rv));
        scl.push(key(t, sv));
    }
    let mut out = layer.clone();
    put(&mut out, StudioLayerProperty::Anchor, anchor);
    put(&mut out, StudioLayerProperty::Position, pos);
    if axes.rotation {
        put(&mut out, StudioLayerProperty::Rotation, rot);
    }
    if axes.scale {
        put(&mut out, StudioLayerProperty::Scale, scl);
    }
    Ok(out)
}
