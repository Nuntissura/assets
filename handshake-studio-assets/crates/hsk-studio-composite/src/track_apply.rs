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
    StudioCompositionKeyframe, StudioCompositionLayer, StudioCornerPin, StudioInterpolation,
    StudioLayerProperty, StudioPropertyStream, corner_quad_is_convex,
};
use hsk_studio_pigment::track_math::{PlanarFrame, PlanarStatus, SimilarityFrame, TrackSample};
use hsk_studio_pigment::transform_math::{LayerTransform, Projective2, ResampleFilter, TransformError};
use std::fmt;

/// RETIRED by lane SMB-AE-KEY-TRACK-3 (the adapter renders corner pins through
/// `SceneContent::Pinned`); no longer returned, kept so callers matching it still compile.
pub const COMPOSITION_CORNER_PIN: &str = "composition_corner_pin";
/// A corner pin on a layer whose content has no pixels of its own (null, adjustment) or on a
/// collapsed precomposition: refused by the composition adapter and by the scene lowering.
pub const COMPOSITION_CORNER_PIN_CONTENT: &str = "composition_corner_pin_content";
/// A corner-pinned layer on the document-graph path (`lower_frame`), whose layer state is an affine
/// matrix: refused typed; `scene_at` + `lower_scene` render it.
pub const COMPOSITION_CORNER_PIN_ON_DOCUMENT_GRAPH: &str = "composition_corner_pin_on_document_graph";

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

// ---- corner pin (lane SMB-AE-KEY-TRACK-2) ----------------------------------------------------

/// A quad of four corners (upper left, upper right, lower right, lower left).
pub type Quad = [[f64; 2]; 4];

/// Corner-pin frames from four point tracks (corner order as [`Quad`]): the frames where all four
/// points are tracked, with their positions.
pub fn corner_frames_from_tracks(corners: [&[TrackSample]; 4]) -> Result<Vec<(usize, Quad)>, TrackApplyError> {
    let n = corners[0].len();
    if corners.iter().any(|c| c.len() != n) {
        return Err(TrackApplyError::Parameter);
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let s = corners.map(|c| c[i]);
        if s.iter().any(|x| x.frame != s[0].frame) {
            return Err(TrackApplyError::Parameter);
        }
        if s.iter().all(|x| x.status.is_tracked()) {
            out.push((s[0].frame, s.map(|x| x.position)));
        }
    }
    Ok(out)
}

/// Corner-pin frames from a planar track: the mapped quad of every `Tracked` frame.
pub fn corner_frames_from_planar(planar: &[PlanarFrame]) -> Vec<(usize, Quad)> {
    planar
        .iter()
        .filter(|f| f.status == PlanarStatus::Tracked)
        .map(|f| (f.frame, f.quad))
        .collect()
}

/// Writes a corner pin onto `target` (a new record; the input is untouched): `from` (the target's
/// layer-space quad, usually its content rectangle) is pinned to each frame's quad plus `offset`,
/// one linear keyframe per frame at its tick. Every quad must be convex and finite, frames
/// strictly increasing, at most [`MAX_TRACK_KEYS`].
pub fn apply_corner_pin(
    target: &StudioCompositionLayer,
    from: Quad,
    frames: &[(usize, Quad)],
    timing: TrackTiming,
    offset: [f64; 2],
) -> Result<StudioCompositionLayer, TrackApplyError> {
    finite(&offset)?;
    if frames.is_empty() || !corner_quad_is_convex(&from) {
        return Err(TrackApplyError::Parameter);
    }
    if frames.len() > MAX_TRACK_KEYS {
        return Err(TrackApplyError::KeyLimit);
    }
    if frames.windows(2).any(|w| w[0].0 >= w[1].0) {
        return Err(TrackApplyError::Parameter);
    }
    let mut keys = Vec::with_capacity(frames.len());
    for (frame, quad) in frames {
        let to = quad.map(|p| [p[0] + offset[0], p[1] + offset[1]]);
        if !corner_quad_is_convex(&to) {
            return Err(TrackApplyError::Parameter);
        }
        keys.push(key(timing.tick(*frame)?, to.iter().flatten().copied().collect()));
    }
    let static_value = keys.first().map(|k| k.value.clone()).unwrap_or_default();
    let mut out = target.clone();
    out.corner_pin = Some(StudioCornerPin { from, static_value, keyframes: keys });
    Ok(out)
}

/// The pigment layer transform of a corner pin with the quad `to` (layer space = the plan
/// coordinates of the layer's tile): the projective map taking `from` onto `to`
/// (`Projective2::from_quads`), executed by render-cpu and render-gpu through pigment's resampler.
pub fn corner_pin_transform(from: Quad, to: Quad, filter: ResampleFilter) -> Result<LayerTransform, TransformError> {
    if !corner_quad_is_convex(&from) || !corner_quad_is_convex(&to) {
        return Err(TransformError::Singular);
    }
    Ok(LayerTransform::projective(Projective2::from_quads(from, to)?, filter))
}

/// The pinned quad of `pin` at composition `tick`, sampled by motion (linear and Bezier keys, held
/// ends; motion owns keyframe evaluation).
#[cfg(feature = "motion")]
pub fn corner_pin_quad_at(pin: &StudioCornerPin, tick: i64) -> Result<Quad, TrackApplyError> {
    use hsk_studio_motion::{EvalTick, Interpolation, Keyframe, Property, TemporalTangent};
    let interp = |i: StudioInterpolation| match i {
        StudioInterpolation::Linear => Interpolation::Linear,
        StudioInterpolation::Bezier => Interpolation::Bezier,
        StudioInterpolation::ContinuousBezier => Interpolation::ContinuousBezier,
        StudioInterpolation::AutoBezier => Interpolation::AutoBezier,
        StudioInterpolation::Hold => Interpolation::Hold,
    };
    let tangents = |t: &[hsk_studio_folio::StudioTemporalTangent]| -> Vec<TemporalTangent> {
        t.iter().map(|t| TemporalTangent { speed: t.speed, influence: t.influence }).collect()
    };
    let keys = pin
        .keyframes
        .iter()
        .map(|k| Keyframe {
            time: hsk_studio_accord::Ticks::new(k.time),
            value: k.value.clone(),
            in_interp: interp(k.in_interp),
            out_interp: interp(k.out_interp),
            in_tangent: tangents(&k.in_tangent),
            out_tangent: tangents(&k.out_tangent),
        })
        .collect();
    let p = Property::new("corner_pin", pin.static_value.clone(), keys).map_err(|_| TrackApplyError::Parameter)?;
    if p.dimension() != StudioCornerPin::DIMENSION {
        return Err(TrackApplyError::Parameter);
    }
    let v = p
        .sample(EvalTick::new(tick), &hsk_studio_accord::CancellationToken::default())
        .map_err(|_| TrackApplyError::Parameter)?;
    Ok([[v[0], v[1]], [v[2], v[3]], [v[4], v[5]], [v[6], v[7]]])
}

// ---- planar track -> mask path (lane SMB-AE-KEY-TRACK-3) ------------------------------------

/// Writes a planar track as an animated mask path on `target` (a new record; the input is
/// untouched): `path` is the start-frame mask (layer space, six numbers per vertex: `x, y, in_dx,
/// in_dy, out_dx, out_dy`), mapped through each `Tracked` frame's homography (a vertex `v` to
/// `H(v)`, a handle `t` to `H(v + t) - H(v)`), one linear keyframe per frame at its tick (STU-CMP-065
/// "Planar / mask tracking ... producing an animated mask path"). A mask with the same id keeps
/// its `inverted` flag and its feather, opacity and expansion streams (lane SMB-AE-MASK-FEATHER-2;
/// Studio reading: a track apply replaces the path, its keyframes and the mode only; After Effects'
/// behaviour is UNVERIFIED, D5); otherwise the mask is appended with those absent (not inverted,
/// no feather, opacity 1, no expansion). Layer space is the tracked footage's pixel space (the
/// footage placed at the layer origin).
pub fn apply_planar_mask(
    target: &StudioCompositionLayer,
    mask_id: &str,
    mode: hsk_studio_folio::StudioCompositionMaskMode,
    path: &[f64],
    planar: &[PlanarFrame],
    timing: TrackTiming,
) -> Result<StudioCompositionLayer, TrackApplyError> {
    let vertices = path.len().checked_div(6).ok_or(TrackApplyError::Parameter)?;
    if path.len().checked_rem(6) != Some(0)
        || !(1..=hsk_studio_folio::MAX_MASK_PATH_VERTICES).contains(&vertices)
        || !path.iter().all(|v| v.is_finite())
        || mask_id.is_empty()
    {
        return Err(TrackApplyError::Parameter);
    }
    let frames: Vec<&PlanarFrame> = planar.iter().filter(|f| f.status == PlanarStatus::Tracked).collect();
    if frames.is_empty() {
        return Err(TrackApplyError::Parameter);
    }
    if frames.len() > MAX_TRACK_KEYS {
        return Err(TrackApplyError::KeyLimit);
    }
    if frames.windows(2).any(|w| w[0].frame >= w[1].frame) {
        return Err(TrackApplyError::Parameter);
    }
    let mut keys = Vec::with_capacity(frames.len());
    for f in frames {
        let mut value = Vec::with_capacity(path.len());
        for v in path.chunks_exact(6) {
            let p = f.homography.apply([v[0], v[1]]);
            let i = f.homography.apply([v[0] + v[2], v[1] + v[3]]);
            let o = f.homography.apply([v[0] + v[4], v[1] + v[5]]);
            value.extend_from_slice(&[p[0], p[1], i[0] - p[0], i[1] - p[1], o[0] - p[0], o[1] - p[1]]);
        }
        finite(&value)?;
        keys.push(key(timing.tick(f.frame)?, value));
    }
    let static_value = keys.first().map(|k| k.value.clone()).unwrap_or_default();
    let mut out = target.clone();
    match out.masks.iter_mut().find(|m| m.mask_id == mask_id) {
        // Path, keyframes and mode only; inverted, feather, opacity and expansion are kept.
        Some(m) => {
            m.mode = mode;
            m.closed = true;
            m.static_value = static_value;
            m.keyframes = keys;
        }
        None => out.masks.push(hsk_studio_folio::StudioCompositionMask {
            mask_id: mask_id.to_owned(),
            mode,
            inverted: false,
            closed: true,
            static_value,
            keyframes: keys,
            feather: None,
            opacity: None,
            expansion: None,
        }),
    }
    Ok(out)
}
