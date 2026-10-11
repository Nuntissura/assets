//! Keyframes, independent in/out interpolation and the temporal segment evaluator (STU-MOT-030..034).
//!
//! # Segment model (explicit math; STATUS: derived from spec + public AE scripting semantics, the
//! exact Adobe numeric equivalence is UNVERIFIED and awaits black-box oracle samples)
//!
//! A segment runs from key `k0` (time `t0`, value `v0`) to key `k1` (`t1`, `v1`), `dt = t1 - t0`
//! ticks, `dts = dt / TICKS_PER_SECOND` seconds. The departure side is `k0.out_interp`, the arrival
//! side is `k1.in_interp` (STU-MOT-030a: independent).
//!
//! * `k0.out_interp == Hold` (or the property is not interpolable, MOT-031b): step, value `v0`.
//! * both sides non-Bezier (Linear, or Hold on the arrival side): `v0 + (v1 - v0) * u`.
//! * otherwise a cubic Bezier in normalised (time, value) space with `u = (t - t0) / dt`:
//!   `X = [0, io, 1 - ii, 1]`, `Y = [v0, v0 + s0*io*dts, v1 - s1*ii*dts, v1]` where `(s0, io)` is
//!   the out tangent (speed in value units per second, influence as a fraction of `dt`) of `k0` and
//!   `(s1, ii)` the in tangent of `k1`. `x(s) = u` is solved by 64 fixed bisection steps on
//!   `s in [0, 1]` (x is monotone, checked at construction), then `value = y(s)`.
//! * A non-Bezier side inside a mixed segment contributes a chord handle: speed equal to the chord
//!   slope `(v1 - v0) / dts`, influence `1/3` (UNVERIFIED against Adobe, deterministic by rule).
//!
//! `ContinuousBezier` / `AutoBezier` evaluate from their stored tangents exactly like `Bezier`:
//! continuity and auto derivation are authoring-time operations (MOT-037/038, later) that produce
//! stored tangents; the evaluator never derives them. Tangents are per component (STU-MOT-033);
//! a single stored tangent broadcasts to all components (covers a spatial path-speed ease).
use crate::error::MotionError;
use hsk_studio_accord::{TICKS_PER_SECOND, Ticks};

/// STU-MOT-034a: default ease influence, stored as a fraction of the segment duration.
pub const DEFAULT_INFLUENCE: f64 = 0.16666666666;
pub const MAX_DIMENSION: usize = 64;
pub const MAX_KEYFRAMES: usize = 65_536;
const CHORD_INFLUENCE: f64 = 1.0 / 3.0;
const BISECTION_STEPS: u32 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    Linear,
    Bezier,
    ContinuousBezier,
    AutoBezier,
    Hold,
}

impl Interpolation {
    pub const fn is_bezier(self) -> bool {
        matches!(self, Self::Bezier | Self::ContinuousBezier | Self::AutoBezier)
    }

    pub const fn wire(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Bezier => "bezier",
            Self::ContinuousBezier => "continuous_bezier",
            Self::AutoBezier => "auto_bezier",
            Self::Hold => "hold",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "linear" => Self::Linear,
            "bezier" => Self::Bezier,
            "continuous_bezier" => Self::ContinuousBezier,
            "auto_bezier" => Self::AutoBezier,
            "hold" => Self::Hold,
            _ => return None,
        })
    }
}

/// STU-MOT-033: `speed` in value units per second, `influence` a fraction `0..=1` of the segment
/// duration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TemporalTangent {
    pub speed: f64,
    pub influence: f64,
}

impl TemporalTangent {
    /// Default ease: zero speed, `DEFAULT_INFLUENCE`.
    pub const EASE: Self = Self {
        speed: 0.0,
        influence: DEFAULT_INFLUENCE,
    };

    pub fn new(speed: f64, influence: f64) -> Result<Self, MotionError> {
        let tangent = Self { speed, influence };
        tangent.validate()?;
        Ok(tangent)
    }

    pub fn validate(&self) -> Result<(), MotionError> {
        if !self.speed.is_finite() {
            return Err(MotionError::NonFinite);
        }
        if !self.influence.is_finite() || !(0.0..=1.0).contains(&self.influence) {
            return Err(MotionError::InvalidInfluence);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Keyframe {
    pub time: Ticks,
    pub value: Vec<f64>,
    pub in_interp: Interpolation,
    pub out_interp: Interpolation,
    pub in_tangent: Vec<TemporalTangent>,
    pub out_tangent: Vec<TemporalTangent>,
}

impl Keyframe {
    /// Linear/linear key with no tangents.
    pub fn new(time: Ticks, value: Vec<f64>) -> Self {
        Self {
            time,
            value,
            in_interp: Interpolation::Linear,
            out_interp: Interpolation::Linear,
            in_tangent: Vec::new(),
            out_tangent: Vec::new(),
        }
    }

    pub fn with_interpolation(mut self, in_interp: Interpolation, out_interp: Interpolation) -> Self {
        self.in_interp = in_interp;
        self.out_interp = out_interp;
        self
    }

    pub fn with_tangents(
        mut self,
        in_tangent: Vec<TemporalTangent>,
        out_tangent: Vec<TemporalTangent>,
    ) -> Self {
        self.in_tangent = in_tangent;
        self.out_tangent = out_tangent;
        self
    }

    /// Default ease on both sides for every component (Bezier/Bezier, `TemporalTangent::EASE`).
    pub fn eased(time: Ticks, value: Vec<f64>) -> Self {
        let tangents = vec![TemporalTangent::EASE];
        Self::new(time, value)
            .with_interpolation(Interpolation::Bezier, Interpolation::Bezier)
            .with_tangents(tangents.clone(), tangents)
    }
}

fn tangent_for(tangents: &[TemporalTangent], component: usize) -> Option<TemporalTangent> {
    match tangents.len() {
        0 => None,
        1 => tangents.first().copied(),
        _ => tangents.get(component).copied(),
    }
}

/// Validates the key-local shape for a property of `dimension` components.
pub(crate) fn validate_key(key: &Keyframe, dimension: usize) -> Result<(), MotionError> {
    if key.value.len() != dimension {
        return Err(MotionError::ComponentMismatch);
    }
    if key.value.iter().any(|v| !v.is_finite()) {
        return Err(MotionError::NonFinite);
    }
    for (interp, tangents) in [
        (key.in_interp, &key.in_tangent),
        (key.out_interp, &key.out_tangent),
    ] {
        if !tangents.is_empty() && tangents.len() != 1 && tangents.len() != dimension {
            return Err(MotionError::ComponentMismatch);
        }
        for tangent in tangents {
            tangent.validate()?;
        }
        if interp.is_bezier() && tangents.is_empty() {
            return Err(MotionError::MissingTangent);
        }
    }
    Ok(())
}

/// Exact whole-tick span of a segment in seconds; `dt >= 1` is guaranteed by key ordering.
fn segment_seconds(dt: i64) -> f64 {
    dt as f64 / TICKS_PER_SECOND as f64
}

#[derive(Clone, Copy, Debug)]
struct Cubic {
    x1: f64,
    x2: f64,
    y: [f64; 4],
}

enum Shape {
    Step,
    Lerp,
    Cubic(Cubic),
}

fn shape(k0: &Keyframe, k1: &Keyframe, component: usize, dts: f64, interpolable: bool) -> Shape {
    if !interpolable || k0.out_interp == Interpolation::Hold {
        return Shape::Step;
    }
    let out_bezier = k0.out_interp.is_bezier();
    let in_bezier = k1.in_interp.is_bezier();
    if !out_bezier && !in_bezier {
        return Shape::Lerp;
    }
    let (v0, v1) = (k0.value[component], k1.value[component]);
    let slope = (v1 - v0) / dts;
    let out = tangent_for(&k0.out_tangent, component).filter(|_| out_bezier);
    let arrive = tangent_for(&k1.in_tangent, component).filter(|_| in_bezier);
    let (s0, io) = out.map_or((slope, CHORD_INFLUENCE), |t| (t.speed, t.influence));
    let (s1, ii) = arrive.map_or((slope, CHORD_INFLUENCE), |t| (t.speed, t.influence));
    Shape::Cubic(Cubic {
        x1: io,
        x2: 1.0 - ii,
        y: [v0, v0 + s0 * io * dts, v1 - s1 * ii * dts, v1],
    })
}

/// x'(s)/3 = a(1-s)^2 + 2b s(1-s) + c s^2 with a = io, b = (1-ii) - io, c = ii; with a, c >= 0 this
/// is non-negative on [0, 1] iff b >= 0 or b^2 <= a*c. For influences in `0..=1` this always holds
/// (hand proof: (io+ii-1)^2 - io*ii is convex and 0 at the three corners of the io+ii > 1 triangle),
/// so `NonMonotoneTime` is a defensive invariant guard against future relaxation of the range.
fn time_curve_monotone(cubic: &Cubic) -> bool {
    let a = cubic.x1;
    let b = cubic.x2 - cubic.x1;
    let c = 1.0 - cubic.x2;
    b >= 0.0 || b * b <= a * c
}

fn bezier(p: [f64; 4], s: f64) -> f64 {
    let m = 1.0 - s;
    m * m * m * p[0] + 3.0 * m * m * s * p[1] + 3.0 * m * s * s * p[2] + s * s * s * p[3]
}

fn solve_cubic(cubic: &Cubic, u: f64) -> f64 {
    let x = [0.0, cubic.x1, cubic.x2, 1.0];
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..BISECTION_STEPS {
        let mid = 0.5 * (lo + hi);
        if bezier(x, mid) < u {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bezier(cubic.y, 0.5 * (lo + hi))
}

/// Rejects segments whose time curve is not monotone (checked for every component).
pub(crate) fn validate_segment(
    k0: &Keyframe,
    k1: &Keyframe,
    dt: i64,
    interpolable: bool,
) -> Result<(), MotionError> {
    let dts = segment_seconds(dt);
    for component in 0..k0.value.len() {
        if let Shape::Cubic(cubic) = shape(k0, k1, component, dts, interpolable) {
            if !time_curve_monotone(&cubic) {
                return Err(MotionError::NonMonotoneTime);
            }
            if cubic.y.iter().any(|v| !v.is_finite()) {
                return Err(MotionError::NonFinite);
            }
        }
    }
    Ok(())
}

/// Value of one segment at `tick` with `t0 <= tick < t1`. `keys` are validated.
pub(crate) fn sample_segment(
    k0: &Keyframe,
    k1: &Keyframe,
    t0: i64,
    t1: i64,
    tick: i64,
    interpolable: bool,
) -> Result<Vec<f64>, MotionError> {
    let dt = i128::from(t1) - i128::from(t0);
    let elapsed = i128::from(tick) - i128::from(t0);
    let dt_i64 = i64::try_from(dt).map_err(|_| MotionError::TickOverflow)?;
    let u = elapsed as f64 / dt as f64;
    let dts = segment_seconds(dt_i64);
    let mut out = Vec::with_capacity(k0.value.len());
    for component in 0..k0.value.len() {
        let v0 = k0.value[component];
        let v1 = k1.value[component];
        let value = if elapsed == 0 {
            v0
        } else {
            match shape(k0, k1, component, dts, interpolable) {
                Shape::Step => v0,
                Shape::Lerp => v0 + (v1 - v0) * u,
                Shape::Cubic(cubic) => solve_cubic(&cubic, u),
            }
        };
        if !value.is_finite() {
            return Err(MotionError::NonFinite);
        }
        out.push(value);
    }
    Ok(out)
}
