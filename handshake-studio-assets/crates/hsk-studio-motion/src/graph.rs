//! Graph editor model (lane SMB-AE-MOTION-TOOLS slice 1; STU-MOT-050..053, STU-MOT-055,
//! STU-MOT-010a `dimensions_separated`, STU-MOT-039).
//!
//! The graph editor is a projection of the property tree (STU-MOT-053): everything here reads or
//! writes `Keyframe` fields; there is no graph-only data.
//!
//! * VALUE graph: `y = value(t)` per component, sampled at caller-chosen ticks.
//! * SPEED graph: `y = dvalue/dt` in value units per second (STU-MOT-051). Per component for a
//!   separated (or scalar) property; for an unseparated multi-component property the graph shows
//!   the magnitude `sqrt(sum_c speed_c^2)` (After Effects' path speed for spatial streams; the AE
//!   numeric equivalence is UNVERIFIED, D5). At a key time the OUTGOING segment's speed is
//!   reported (the same half-open `[t0, t1)` rule as `Property::sample`); before the first key and
//!   from the last key on the speed is 0 (held ends); a hold segment has speed 0.
//! * Separate / join dimensions (STU-MOT-010a: explicit typed conversions with a stated rule).
//!   Separate is exact: component `c` keeps every key time, its value, both interpolations and its
//!   own temporal tangent (a single stored tangent broadcasts). Join takes the union of the
//!   component key times; a component without a key at a union time gets an inserted key carrying
//!   its sampled value and its incoming/outgoing sampled speed with the default influence
//!   (STU-MOT-034); interpolation per side: hold when every component holds there, linear when every
//!   component is linear there, else bezier (a non-bezier component side contributes its sampled
//!   speed). Join is therefore lossy where key times differ (inserted keys re-split segments whose
//!   influences are fractions of the segment duration): the stated conversion of STU-MOT-010a.
//! * Velocity record (STU-MOT-055): per component incoming/outgoing speed and influence plus the
//!   continuity toggle, read and written headlessly.
use crate::{
    error::MotionError,
    keyframe::{DEFAULT_INFLUENCE, Interpolation, Keyframe, MAX_KEYFRAMES, TemporalTangent, segment_end_speed, segment_speed},
    property::Property,
    time::EvalTick,
};
use hsk_studio_accord::{CancellationToken, Ticks};

/// Largest number of points one graph request may produce.
pub const MAX_GRAPH_SAMPLES: usize = 1 << 16;

/// The two graph types of STU-MOT-051.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphKind {
    Value,
    Speed,
}

/// Which curve of a property is graphed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphChannel {
    /// One component (separated dimensions, or a scalar).
    Component(usize),
    /// Unseparated multi-component property: speed magnitude over all components (value graphs
    /// of an unseparated property still graph one component; see module docs).
    Magnitude,
}

/// One graph point: exact tick and its ordinate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GraphPoint {
    pub tick: i64,
    pub y: f64,
}

/// Index of the segment containing `tick` (`None` = before the first key or from the last key on).
fn segment_of(property: &Property, tick: i64) -> Option<usize> {
    let ticks = property.key_ticks();
    let after = ticks.partition_point(|t| *t <= tick);
    (after > 0 && after < ticks.len()).then(|| after - 1)
}

/// Speed of one component at `tick` (module docs).
pub fn speed_at(property: &Property, component: usize, tick: EvalTick) -> Result<f64, MotionError> {
    if component >= property.dimension() {
        return Err(MotionError::ComponentMismatch);
    }
    let Some(i) = segment_of(property, tick.value()) else {
        return Ok(0.0);
    };
    let keys = property.keyframes();
    let ticks = property.key_ticks();
    segment_speed(
        &keys[i],
        &keys[i + 1],
        ticks[i],
        ticks[i + 1],
        tick.value(),
        property.interpolable(),
        component,
    )
}

/// Incoming speed at `tick`: at a key time, the analytic end speed of the segment ARRIVING at that
/// key (0 at the first key); elsewhere the ordinary speed (continuous inside a segment).
fn incoming_speed_at(property: &Property, component: usize, tick: i64) -> Result<f64, MotionError> {
    let ticks = property.key_ticks();
    match ticks.binary_search(&tick) {
        Ok(0) => Ok(0.0),
        Ok(j) => {
            let keys = property.keyframes();
            segment_end_speed(&keys[j - 1], &keys[j], ticks[j - 1], ticks[j], property.interpolable(), component)
        }
        Err(_) => speed_at(property, component, EvalTick::new(tick)),
    }
}

/// Speed magnitude over all components at `tick`.
pub fn path_speed_at(property: &Property, tick: EvalTick) -> Result<f64, MotionError> {
    let mut sum = 0.0;
    for c in 0..property.dimension() {
        let s = speed_at(property, c, tick)?;
        sum += s * s;
    }
    Ok(sum.sqrt())
}

/// Samples a graph at `samples` evenly spaced ticks over `[from, to]` (both ends included; tick
/// `from + round_half_up(i * (to - from) / (samples - 1))`). `samples` is checked against
/// [`MAX_GRAPH_SAMPLES`] before anything is allocated.
pub fn graph(
    property: &Property,
    kind: GraphKind,
    channel: GraphChannel,
    from: EvalTick,
    to: EvalTick,
    samples: usize,
    cancel: &CancellationToken,
) -> Result<Vec<GraphPoint>, MotionError> {
    if !(2..=MAX_GRAPH_SAMPLES).contains(&samples) {
        return Err(MotionError::SampleLimit);
    }
    if from.value() > to.value() {
        return Err(MotionError::InvalidBounds);
    }
    if let GraphChannel::Component(c) = channel
        && c >= property.dimension()
    {
        return Err(MotionError::ComponentMismatch);
    }
    let span = i128::from(to.value()) - i128::from(from.value());
    let last = (samples - 1) as i128;
    let mut out = Vec::with_capacity(samples);
    for i in 0..samples {
        cancel.check().map_err(|_| MotionError::Canceled)?;
        let num = span * i as i128;
        let offset = (2 * num + last) / (2 * last);
        let tick = i64::try_from(i128::from(from.value()) + offset).map_err(|_| MotionError::TickOverflow)?;
        let at = EvalTick::new(tick);
        let y = match (kind, channel) {
            (GraphKind::Value, GraphChannel::Component(c)) => property.sample(at, cancel)?[c],
            (GraphKind::Value, GraphChannel::Magnitude) => {
                property.sample(at, cancel)?.iter().map(|v| v * v).sum::<f64>().sqrt()
            }
            (GraphKind::Speed, GraphChannel::Component(c)) => speed_at(property, c, at)?,
            (GraphKind::Speed, GraphChannel::Magnitude) => path_speed_at(property, at)?,
        };
        out.push(GraphPoint { tick, y });
    }
    Ok(out)
}

/// Auto-fit of the vertical axis over curves (STU-MOT-052): `(min, max)` of every point, `None` when
/// there are no points.
pub fn fit_range(curves: &[&[GraphPoint]]) -> Option<(f64, f64)> {
    let mut range: Option<(f64, f64)> = None;
    for p in curves.iter().flat_map(|c| c.iter()) {
        range = Some(match range {
            None => (p.y, p.y),
            Some((lo, hi)) => (lo.min(p.y), hi.max(p.y)),
        });
    }
    range
}

fn tangent_of(tangents: &[TemporalTangent], component: usize) -> Vec<TemporalTangent> {
    match tangents.len() {
        0 => Vec::new(),
        1 => vec![tangents[0]],
        _ => tangents.get(component).copied().into_iter().collect(),
    }
}

/// Separate dimensions (exact; module docs). Component property keys are `<key>_<index>`.
pub fn separate_dimensions(property: &Property) -> Result<Vec<Property>, MotionError> {
    let mut out = Vec::with_capacity(property.dimension());
    for c in 0..property.dimension() {
        let keys = property
            .keyframes()
            .iter()
            .map(|k| Keyframe {
                time: k.time,
                value: vec![k.value[c]],
                in_interp: k.in_interp,
                out_interp: k.out_interp,
                in_tangent: tangent_of(&k.in_tangent, c),
                out_tangent: tangent_of(&k.out_tangent, c),
            })
            .collect();
        let key = format!("{}_{c}", property.key());
        let (min, max) = property.bounds();
        out.push(
            Property::new(&key, vec![property.static_value()[c]], keys)?
                .with_bounds(min, max)?
                .with_interpolable(property.interpolable())?,
        );
    }
    Ok(out)
}

fn side_interp(sides: &[Interpolation]) -> Interpolation {
    if sides.iter().all(|i| *i == Interpolation::Hold) {
        Interpolation::Hold
    } else if sides.iter().all(|i| *i == Interpolation::Linear) {
        Interpolation::Linear
    } else {
        Interpolation::Bezier
    }
}

/// Join separated component properties (module docs; lossy where key times differ).
pub fn join_dimensions(key: &str, components: &[Property]) -> Result<Property, MotionError> {
    if components.is_empty() || components.len() > crate::keyframe::MAX_DIMENSION {
        return Err(MotionError::DimensionLimit);
    }
    if components.iter().any(|p| p.dimension() != 1) {
        return Err(MotionError::ComponentMismatch);
    }
    let total: usize = components.iter().map(|p| p.keyframes().len()).sum();
    if total > MAX_KEYFRAMES.saturating_mul(components.len()) {
        return Err(MotionError::KeyframeLimit);
    }
    let mut times: Vec<i64> = components.iter().flat_map(|p| p.key_ticks().iter().copied()).collect();
    times.sort_unstable();
    times.dedup();
    if times.len() > MAX_KEYFRAMES {
        return Err(MotionError::KeyframeLimit);
    }
    let cancel = CancellationToken::default();
    let mut keys = Vec::with_capacity(times.len());
    for &t in &times {
        let mut value = Vec::with_capacity(components.len());
        let (mut ins, mut outs) = (Vec::new(), Vec::new());
        let (mut in_t, mut out_t) = (Vec::new(), Vec::new());
        for p in components {
            let found = p.key_ticks().binary_search(&t).ok().map(|i| &p.keyframes()[i]);
            match found {
                Some(k) => {
                    value.push(k.value[0]);
                    ins.push(k.in_interp);
                    outs.push(k.out_interp);
                    let ti = k.in_tangent.first().copied();
                    let to = k.out_tangent.first().copied();
                    in_t.push(match ti {
                        Some(t) => t,
                        None => TemporalTangent { speed: incoming_speed_at(p, 0, t)?, influence: DEFAULT_INFLUENCE },
                    });
                    out_t.push(match to {
                        Some(t) => t,
                        None => TemporalTangent { speed: speed_at(p, 0, EvalTick::new(t))?, influence: DEFAULT_INFLUENCE },
                    });
                }
                None => {
                    value.push(p.sample(EvalTick::new(t), &cancel)?[0]);
                    ins.push(Interpolation::Bezier);
                    outs.push(Interpolation::Bezier);
                    in_t.push(TemporalTangent { speed: incoming_speed_at(p, 0, t)?, influence: DEFAULT_INFLUENCE });
                    out_t.push(TemporalTangent { speed: speed_at(p, 0, EvalTick::new(t))?, influence: DEFAULT_INFLUENCE });
                }
            }
        }
        let in_interp = side_interp(&ins);
        let out_interp = side_interp(&outs);
        let time = Ticks::new(u64::try_from(t).map_err(|_| MotionError::TickOverflow)?);
        keys.push(Keyframe {
            time,
            value,
            in_interp,
            out_interp,
            in_tangent: if in_interp.is_bezier() { in_t } else { Vec::new() },
            out_tangent: if out_interp.is_bezier() { out_t } else { Vec::new() },
        });
    }
    let static_value = components.iter().map(|p| p.static_value()[0]).collect();
    Property::new(key, static_value, keys)
}

/// STU-MOT-055 velocity record of one key: per component incoming speed/influence, outgoing
/// speed/influence, and the continuity toggle.
#[derive(Clone, Debug, PartialEq)]
pub struct VelocityRecord {
    pub incoming: Vec<TemporalTangent>,
    pub outgoing: Vec<TemporalTangent>,
    /// Continuity: incoming speed equals outgoing speed on every component (continuous bezier).
    pub continuous: bool,
}

/// Reads the velocity record of key `index`; a non-bezier side reports its effective speed (the
/// analytic speed at the key) with the default influence.
pub fn velocity_record(property: &Property, index: usize) -> Result<VelocityRecord, MotionError> {
    let key = property.keyframes().get(index).ok_or(MotionError::SelectionOutOfRange)?;
    let t = property.key_ticks()[index];
    let dims = property.dimension();
    let mut incoming = Vec::with_capacity(dims);
    let mut outgoing = Vec::with_capacity(dims);
    for c in 0..dims {
        incoming.push(match key.in_interp.is_bezier() {
            true => tangent_of(&key.in_tangent, c)[0],
            false => TemporalTangent { speed: incoming_speed_at(property, c, t)?, influence: DEFAULT_INFLUENCE },
        });
        outgoing.push(match key.out_interp.is_bezier() {
            true => tangent_of(&key.out_tangent, c)[0],
            false => TemporalTangent { speed: speed_at(property, c, EvalTick::new(t))?, influence: DEFAULT_INFLUENCE },
        });
    }
    let continuous = key.in_interp == Interpolation::ContinuousBezier
        || key.in_interp == Interpolation::AutoBezier
        || incoming.iter().zip(&outgoing).all(|(a, b)| a.speed == b.speed);
    Ok(VelocityRecord { incoming, outgoing, continuous })
}

/// Writes the velocity record of key `index`: both sides become bezier with the given per
/// component tangents (one entry broadcasts); `continuous` stores `continuous_bezier` on both sides
/// and requires equal incoming and outgoing speeds (refused `invalid_parameter` otherwise; the
/// record never silently rewrites a speed). A manual edit of an `auto_bezier` key converts it to
/// `continuous_bezier` or `bezier` (STU-MOT-031).
pub fn set_velocity(property: &mut Property, index: usize, record: &VelocityRecord) -> Result<(), MotionError> {
    let mut keys = property.keyframes().to_vec();
    let key = keys.get_mut(index).ok_or(MotionError::SelectionOutOfRange)?;
    if record.continuous {
        let n = record.incoming.len().max(record.outgoing.len());
        for c in 0..n {
            let a = tangent_of(&record.incoming, c);
            let b = tangent_of(&record.outgoing, c);
            if a.first().map(|t| t.speed) != b.first().map(|t| t.speed) {
                return Err(MotionError::InvalidParameter { parameter: "continuity_speed" });
            }
        }
    }
    let interp = if record.continuous { Interpolation::ContinuousBezier } else { Interpolation::Bezier };
    key.in_interp = interp;
    key.out_interp = interp;
    key.in_tangent = record.incoming.clone();
    key.out_tangent = record.outgoing.clone();
    property.set_keyframes(keys)
}
