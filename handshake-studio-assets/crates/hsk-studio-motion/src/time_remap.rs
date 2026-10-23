//! Time remapping, time stretch / reverse and the frame-blending sample rule (lane
//! SMB-AE-ROTO-TIMEREMAP slice 1; parity AE-10).
//!
//! Spec basis (module 14): [STU-MOT-045] "Time remapping turns a layer's source time into a
//! keyframable property ... a `time_remap` property whose value AT a composition time is the SOURCE
//! time to sample" with the normative contract 1-7 (two keys on enable, ordinary property,
//! decreasing = backwards / flat = freeze, out point unbounded, out-of-range values clamp to the
//! first or last frame, frame blending between frames, one mechanism for clip speed);
//! [STU-MOT-007a] "The frame-blending enumeration is normative: `off`, `frame_mix`,
//! `pixel_motion`"; STU-MOT-006 `stretch` "rational | Time-stretch factor; negative reverses".
//!
//! Model (Studio reading; After Effects numeric behaviour UNVERIFIED, D5):
//! - The remap is an ordinary one-component motion [`Property`] keyed [`TIME_REMAP_KEY`] whose
//!   value is source time in SECONDS; every keyframe operation, interpolation type and expression
//!   applies to it ([`TimeRemap::property_mut`]).
//! - [`TimeRemap::enable`]: key 1 = (in point, 0 s), key 2 = (in point + last-frame start, last
//!   frame start in seconds), linear: the remap equals `tick - in` everywhere in the layer, so
//!   enabling changes nothing (test `enable_is_identity_and_clamps_hostile_values`).
//! - [`TimeRemap::from_stretch`]: the same two keys over `|stretch|` times the source span;
//!   a negative stretch swaps the values (reverse).
//! - [`TimeRemap::reverse`]: time-reverse keyframes (times mirrored inside the first..last span,
//!   order reversed, in/out interpolation and tangents swapped, speeds negated).
//! - [`TimeRemap::sample`]: the property value, clamped to `[0, last frame start]` (contract 5,
//!   reported in [`RemapSample::clamp`], never an error), then rounded to whole source ticks.
//! - [`frame_sample`]: source tick -> the frame pair and weight. `off` = the frame containing the
//!   tick (floor); `frame_mix` / `pixel_motion` = frames `floor` and `floor + 1` (held at the last
//!   frame) with weight `remainder / frame duration`. The pixel work belongs to Pigment
//!   (`pigment::frame_blend`); Motion only decides which frames and how much of each.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::MotionError;
use crate::keyframe::Keyframe;
use crate::property::Property;
use crate::time::EvalTick;
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND, Ticks};

/// Property key of the remap stream (STU-MOT-045; the property list also names `timeRemap`).
pub const TIME_REMAP_KEY: &str = "time_remap";

/// The STU-MOT-007a frame-blending enumeration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameBlending {
    #[default]
    Off,
    FrameMix,
    PixelMotion,
}

impl FrameBlending {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::FrameMix => "frame_mix",
            Self::PixelMotion => "pixel_motion",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "off" => Self::Off,
            "frame_mix" => Self::FrameMix,
            "pixel_motion" => Self::PixelMotion,
            _ => return None,
        })
    }
}

/// Source footage timing in Accord ticks: total duration and one frame's duration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceTiming {
    duration: u64,
    frame: u64,
}

impl SourceTiming {
    /// `frame > 0`, `frame <= duration`, both within `i64`.
    pub fn new(duration_ticks: u64, frame_ticks: u64) -> Result<Self, MotionError> {
        if frame_ticks == 0 || frame_ticks > duration_ticks {
            return Err(MotionError::InvalidParameter { parameter: "source_timing" });
        }
        if i64::try_from(duration_ticks).is_err() {
            return Err(MotionError::TickOverflow);
        }
        Ok(Self {
            duration: duration_ticks,
            frame: frame_ticks,
        })
    }

    pub const fn duration_ticks(self) -> u64 {
        self.duration
    }

    pub const fn frame_ticks(self) -> u64 {
        self.frame
    }

    /// Start tick of the last whole frame (`duration - frame`).
    pub const fn last_frame_tick(self) -> u64 {
        self.duration.saturating_sub(self.frame)
    }

    /// Index of the last frame.
    pub fn last_frame_index(self) -> u64 {
        self.last_frame_tick().checked_div(self.frame).unwrap_or(0)
    }

    fn last_frame_seconds(self) -> f64 {
        self.last_frame_tick() as f64 / TICKS_PER_SECOND as f64
    }
}

/// Which side of the source range a remap value was clamped to (STU-MOT-045 contract 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemapClamp {
    None,
    First,
    Last,
}

/// One remap evaluation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RemapSample {
    /// Clamped source time in seconds.
    pub source_seconds: f64,
    /// Clamped source time in whole ticks, `0..=last_frame_tick`.
    pub source_tick: u64,
    pub clamp: RemapClamp,
}

/// The frames to show for one source tick and the weight of the second (`0` = first only).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameSample {
    pub frame_a: u64,
    pub frame_b: u64,
    /// Weight of `frame_b` in `[0, 1)`.
    pub weight_b: f64,
    pub blending: FrameBlending,
}

/// Frame pair of `source_tick` (clamped to the source range) under `blending` (module docs).
pub fn frame_sample(source_tick: u64, source: SourceTiming, blending: FrameBlending) -> FrameSample {
    let t = source_tick.min(source.last_frame_tick());
    let a = t.checked_div(source.frame).unwrap_or(0);
    let r = t.checked_rem(source.frame).unwrap_or(0);
    if blending == FrameBlending::Off || r == 0 {
        return FrameSample {
            frame_a: a,
            frame_b: a,
            weight_b: 0.0,
            blending,
        };
    }
    let b = a.saturating_add(1).min(source.last_frame_index());
    FrameSample {
        frame_a: a,
        frame_b: b,
        weight_b: if b == a { 0.0 } else { r as f64 / source.frame as f64 },
        blending,
    }
}

/// The `time_remap` property of one layer plus its source timing.
#[derive(Clone, Debug)]
pub struct TimeRemap {
    property: Property,
    source: SourceTiming,
}

fn seconds(ticks: u64) -> f64 {
    ticks as f64 / TICKS_PER_SECOND as f64
}

impl TimeRemap {
    /// Enabling time remap on a layer whose in point is `in_tick` (STU-MOT-045 contract 1).
    pub fn enable(in_tick: Ticks, source: SourceTiming) -> Result<Self, MotionError> {
        Self::from_stretch(in_tick, source, 1, 1)
    }

    /// The remap equivalent of a rational time stretch `numerator / denominator` (`denominator >
    /// 0`, `numerator != 0`; negative reverses): the source's first..last frame span over
    /// `|stretch|` times as many composition ticks.
    pub fn from_stretch(in_tick: Ticks, source: SourceTiming, numerator: i64, denominator: i64) -> Result<Self, MotionError> {
        if numerator == 0 || denominator <= 0 {
            return Err(MotionError::InvalidParameter { parameter: "stretch" });
        }
        let span = i128::from(source.last_frame_tick())
            .checked_mul(i128::from(numerator).checked_abs().ok_or(MotionError::TickOverflow)?)
            .and_then(|v| v.checked_div(i128::from(denominator)))
            .ok_or(MotionError::TickOverflow)?;
        let start = i128::from(in_tick.value());
        let end = start.checked_add(span).ok_or(MotionError::TickOverflow)?;
        let end_u = u64::try_from(end).map_err(|_| MotionError::TickOverflow)?;
        i64::try_from(end).map_err(|_| MotionError::TickOverflow)?;
        let last = source.last_frame_seconds();
        let (v0, v1) = if numerator < 0 { (last, 0.0) } else { (0.0, last) };
        let mut keys = vec![Keyframe::new(in_tick, vec![v0])];
        if end_u != in_tick.value() {
            keys.push(Keyframe::new(Ticks::new(end_u), vec![v1]));
        }
        let property = Property::new(TIME_REMAP_KEY, vec![v0], keys)?;
        Ok(Self { property, source })
    }

    /// Remap from caller keys `(composition tick, source seconds)`, linear (refuses non-finite
    /// values, unsorted or out-of-range ticks through [`Property::new`]).
    pub fn with_keys(source: SourceTiming, keys: &[(u64, f64)]) -> Result<Self, MotionError> {
        let first = keys.first().ok_or(MotionError::InvalidParameter { parameter: "time_remap_keys" })?;
        let frames = keys.iter().map(|(t, v)| Keyframe::new(Ticks::new(*t), vec![*v])).collect();
        let property = Property::new(TIME_REMAP_KEY, vec![first.1], frames)?;
        Ok(Self { property, source })
    }

    /// Wraps an authored property (must be keyed [`TIME_REMAP_KEY`], one component).
    pub fn from_property(property: Property, source: SourceTiming) -> Result<Self, MotionError> {
        if property.key() != TIME_REMAP_KEY {
            return Err(MotionError::InvalidKey);
        }
        if property.dimension() != 1 {
            return Err(MotionError::ComponentMismatch);
        }
        Ok(Self { property, source })
    }

    pub fn property(&self) -> &Property {
        &self.property
    }

    /// Every keyframe operation applies to the remap (STU-MOT-045 contract 2).
    pub fn property_mut(&mut self) -> &mut Property {
        &mut self.property
    }

    pub fn source(&self) -> SourceTiming {
        self.source
    }

    /// Time-reverse keyframes (module docs). One key or none: unchanged.
    pub fn reverse(&mut self) -> Result<(), MotionError> {
        let keys = self.property.keyframes();
        let (Some(first), Some(last)) = (keys.first(), keys.last()) else {
            return Ok(());
        };
        let sum = u128::from(first.time.value()).checked_add(u128::from(last.time.value())).ok_or(MotionError::TickOverflow)?;
        let mut out = Vec::with_capacity(keys.len());
        for k in keys.iter().rev() {
            let t = sum.checked_sub(u128::from(k.time.value())).ok_or(MotionError::TickOverflow)?;
            let t = u64::try_from(t).map_err(|_| MotionError::TickOverflow)?;
            let flip = |v: &[crate::keyframe::TemporalTangent]| {
                v.iter()
                    .map(|g| crate::keyframe::TemporalTangent {
                        speed: -g.speed,
                        influence: g.influence,
                    })
                    .collect::<Vec<_>>()
            };
            out.push(Keyframe {
                time: Ticks::new(t),
                value: k.value.clone(),
                in_interp: k.out_interp,
                out_interp: k.in_interp,
                in_tangent: flip(&k.out_tangent),
                out_tangent: flip(&k.in_tangent),
            });
        }
        self.property.set_keyframes(out)
    }

    /// Source time at composition tick `tick`, clamped to the source range (contract 5).
    pub fn sample(&self, tick: EvalTick, cancel: &CancellationToken) -> Result<RemapSample, MotionError> {
        let v = self
            .property
            .sample(tick, cancel)?
            .first()
            .copied()
            .ok_or(MotionError::ComponentMismatch)?;
        if !v.is_finite() {
            return Err(MotionError::NonFinite);
        }
        let last = self.source.last_frame_seconds();
        let (s, clamp) = if v < 0.0 {
            (0.0, RemapClamp::First)
        } else if v > last {
            (last, RemapClamp::Last)
        } else {
            (v, RemapClamp::None)
        };
        let ticks = match clamp {
            RemapClamp::First => 0,
            RemapClamp::Last => self.source.last_frame_tick(),
            // `s` is within [0, last] seconds, so the product is within [0, last_frame_tick] up to
            // one rounding; the integer clamp removes that.
            RemapClamp::None => ((s * TICKS_PER_SECOND as f64).round() as u64).min(self.source.last_frame_tick()),
        };
        Ok(RemapSample {
            source_seconds: s,
            source_tick: ticks,
            clamp,
        })
    }

    /// The frames to show at composition tick `tick` under `blending`.
    pub fn frame_at(&self, tick: EvalTick, blending: FrameBlending, cancel: &CancellationToken) -> Result<FrameSample, MotionError> {
        let s = self.sample(tick, cancel)?;
        Ok(frame_sample(s.source_tick, self.source, blending))
    }
}

/// Seconds of a tick count (exposed for adapters that author remap keys from ticks).
pub fn ticks_to_seconds(ticks: u64) -> f64 {
    seconds(ticks)
}
