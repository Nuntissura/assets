//! Spatial interpolation (lane SMB-AI-AE-ORACLE-FOLLOWUPS slice 3; STU-MOT-036b): a property of two
//! or more components may carry per-key spatial tangents. A segment then moves along the cubic
//! Bezier `P0 = v0, P1 = v0 + out0, P2 = v1 + in1, P3 = v1` (tangents relative to their key value,
//! After Effects' `keyOutSpatialTangent` / `keyInSpatialTangent`), and the temporal interpolation
//! drives the DISTANCE travelled along it: the segment's temporal model (the same evaluator as a
//! one-component stream, `0 -> L` with the path speed as the tangent speed) gives the arc length
//! `s(t)`, and the value is the curve point at arc length `s` (After Effects' path speed).
//!
//! Arc length: 64 equal parameter intervals, each integrated with 5-point Gauss-Legendre (exact for
//! the speed polynomial's smooth part to far below 1e-9 px on the oracle paths); the inverse
//! `s -> u` bisects `u` in 64 fixed steps. Only `+ - * /` and `sqrt` are used, so values are
//! deterministic on every host.
//!
//! Oracle (D5, After Effects 26.5, corpus `adobe-session/aftereffects/AE-02__spatial.json`, test
//! `d5_ae_oracle::ae02_spatial_position`):
//! * Spatial auto-bezier ([`spatial_auto_bezier`]): an interior key's tangents are
//!   `out = (P[i+1] - P[i-1]) / 6`, `in = -out`; the first and last keys get zero tangents. Fitted on
//!   `three_keys_roving_middle` (a scale of 1/6 beats 0.160..0.171 and the alternatives "a third of
//!   each neighbour distance" or neighbour-weighted chords by 0.5 px or more; end tangents pointing
//!   at the neighbour handle miss by 2.2 px); the 2-key readback `[0, 0, 0]` confirms zero end
//!   tangents. More than three keys, non-collinear end behaviour and AE's continuous/auto spatial
//!   toggles beyond this case are UNVERIFIED.
//! * Roving ([`crate::rove`]) uses these arc lengths when the property has spatial tangents.
//! * Residual: After Effects' own arc-length table leaves an along-path jitter of up to 0.01 px
//!   (0.0045 px even on a straight path); the path shape itself agrees to 0.0005 px.
use crate::{
    error::MotionError,
    keyframe::{Interpolation, Keyframe, TemporalTangent, sample_segment},
    property::Property,
};
use hsk_studio_accord::Ticks;

/// Spatial tangents of one key, relative to its value; both vectors have the property's dimension.
#[derive(Clone, Debug, PartialEq)]
pub struct SpatialTangents {
    pub in_tangent: Vec<f64>,
    pub out_tangent: Vec<f64>,
}

impl SpatialTangents {
    /// Zero tangents (a straight path through the key) for `dimension` components.
    pub fn zero(dimension: usize) -> Self {
        Self { in_tangent: vec![0.0; dimension], out_tangent: vec![0.0; dimension] }
    }
}

const TABLE_INTERVALS: usize = 64;
const INVERSE_STEPS: u32 = 64;
/// 5-point Gauss-Legendre nodes on [-1, 1] and weights.
const GL_NODES: [f64; 5] = [
    -0.906_179_845_938_664,
    -0.538_469_310_105_683,
    0.0,
    0.538_469_310_105_683,
    0.906_179_845_938_664,
];
const GL_WEIGHTS: [f64; 5] = [
    0.236_926_885_056_189,
    0.478_628_670_499_366,
    0.568_888_888_888_889,
    0.478_628_670_499_366,
    0.236_926_885_056_189,
];

/// One spatial segment's control points (`dimension` components each).
pub(crate) struct SpatialCurve<'a> {
    p0: &'a [f64],
    p3: &'a [f64],
    out0: &'a [f64],
    in1: &'a [f64],
}

impl<'a> SpatialCurve<'a> {
    pub(crate) fn new(p0: &'a [f64], out0: &'a [f64], in1: &'a [f64], p3: &'a [f64]) -> Self {
        Self { p0, p3, out0, in1 }
    }

    fn point(&self, u: f64) -> Vec<f64> {
        let m = 1.0 - u;
        (0..self.p0.len())
            .map(|c| {
                let (a, d) = (self.p0[c], self.p3[c]);
                let (b, e) = (a + self.out0[c], d + self.in1[c]);
                m * m * m * a + 3.0 * m * m * u * b + 3.0 * m * u * u * e + u * u * u * d
            })
            .collect()
    }

    fn speed(&self, u: f64) -> f64 {
        let m = 1.0 - u;
        let mut sum = 0.0;
        for c in 0..self.p0.len() {
            let (a, d) = (self.p0[c], self.p3[c]);
            let (b, e) = (a + self.out0[c], d + self.in1[c]);
            let derivative = 3.0 * (m * m * (b - a) + 2.0 * m * u * (e - b) + u * u * (d - e));
            sum += derivative * derivative;
        }
        sum.sqrt()
    }

    fn integrate(&self, a: f64, b: f64) -> f64 {
        let (half, mid) = (0.5 * (b - a), 0.5 * (a + b));
        GL_NODES.iter().zip(GL_WEIGHTS).map(|(x, w)| w * self.speed(mid + half * x)).sum::<f64>() * half
    }

    /// Cumulative arc length at the `TABLE_INTERVALS + 1` equally spaced parameters.
    fn table(&self) -> Vec<f64> {
        let mut out = Vec::with_capacity(TABLE_INTERVALS + 1);
        let mut total = 0.0;
        out.push(0.0);
        for i in 0..TABLE_INTERVALS {
            let (a, b) = (i as f64 / TABLE_INTERVALS as f64, (i + 1) as f64 / TABLE_INTERVALS as f64);
            total += self.integrate(a, b);
            out.push(total);
        }
        out
    }

    pub(crate) fn length(&self) -> f64 {
        self.table().last().copied().unwrap_or(0.0)
    }

    /// Point at arc length `s` (clamped to `0..=length`).
    fn at_length(&self, table: &[f64], s: f64) -> Vec<f64> {
        let total = table.last().copied().unwrap_or(0.0);
        if s <= 0.0 || total <= 0.0 {
            return self.point(0.0);
        }
        if s >= total {
            return self.point(1.0);
        }
        let length_to = |u: f64| -> f64 {
            let scaled = u * TABLE_INTERVALS as f64;
            let i = (scaled.floor() as usize).min(TABLE_INTERVALS - 1);
            let start = i as f64 / TABLE_INTERVALS as f64;
            table[i] + self.integrate(start, u)
        };
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for _ in 0..INVERSE_STEPS {
            let mid = 0.5 * (lo + hi);
            if length_to(mid) < s {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        self.point(0.5 * (lo + hi))
    }
}

/// Path speed tangent of a spatial key side: a single stored tangent, or the magnitude of the
/// per-component speeds (influence of component 0).
fn path_tangent(tangents: &[TemporalTangent]) -> Vec<TemporalTangent> {
    match tangents {
        [] => Vec::new(),
        [one] => vec![TemporalTangent { speed: one.speed.abs(), influence: one.influence }],
        many => vec![TemporalTangent {
            speed: many.iter().map(|t| t.speed * t.speed).sum::<f64>().sqrt(),
            influence: many[0].influence,
        }],
    }
}

/// Value of a spatial segment at `tick` (`t0 <= tick < t1`); see module docs.
pub(crate) fn sample_spatial_segment(
    k0: &Keyframe,
    k1: &Keyframe,
    out0: &[f64],
    in1: &[f64],
    t0: i64,
    t1: i64,
    tick: i64,
) -> Result<Vec<f64>, MotionError> {
    let curve = SpatialCurve::new(&k0.value, out0, in1, &k1.value);
    let table = curve.table();
    let total = table.last().copied().unwrap_or(0.0);
    if !total.is_finite() {
        return Err(MotionError::NonFinite);
    }
    // Distance keys: the same temporal model as a one-component stream from 0 to the path length.
    let d0 = Keyframe {
        time: Ticks::new(0),
        value: vec![0.0],
        in_interp: Interpolation::Linear,
        out_interp: k0.out_interp,
        in_tangent: Vec::new(),
        out_tangent: path_tangent(&k0.out_tangent),
    };
    let d1 = Keyframe {
        time: Ticks::new(1),
        value: vec![total],
        in_interp: k1.in_interp,
        out_interp: Interpolation::Linear,
        in_tangent: path_tangent(&k1.in_tangent),
        out_tangent: Vec::new(),
    };
    let s = sample_segment(&d0, &d1, t0, t1, tick, true)?[0];
    let value = curve.at_length(&table, s);
    if value.iter().any(|v| !v.is_finite()) {
        return Err(MotionError::NonFinite);
    }
    Ok(value)
}

/// Spatial auto-bezier (module docs): sets the selected keys' spatial tangents from their
/// neighbours (interior key: `(P[i+1] - P[i-1]) / 6` out, its negative in; first/last key: zero).
/// A property without spatial tangents gets zero tangents on every other key. One revision step;
/// the property is unchanged on refusal.
pub fn spatial_auto_bezier(property: &mut Property, selection: &[usize]) -> Result<(), MotionError> {
    let keys = property.keyframes();
    let n = keys.len();
    if selection.iter().any(|i| *i >= n) {
        return Err(MotionError::SelectionOutOfRange);
    }
    let dimension = property.dimension();
    let mut tangents = match property.spatial_tangents() {
        Some(existing) => existing.to_vec(),
        None => vec![SpatialTangents::zero(dimension); n],
    };
    for &i in selection {
        tangents[i] = if i == 0 || i + 1 == n {
            SpatialTangents::zero(dimension)
        } else {
            let (before, after) = (&keys[i - 1].value, &keys[i + 1].value);
            let out: Vec<f64> = before.iter().zip(after).map(|(a, b)| (b - a) / 6.0).collect();
            SpatialTangents { in_tangent: out.iter().map(|v| -v).collect(), out_tangent: out }
        };
    }
    property.set_spatial_tangents(Some(tangents))
}
