//! Keyframe assistants (lane SMB-AE-MOTION-TOOLS slice 1; STU-MOT-035, STU-MOT-037, STU-MOT-031,
//! STU-MOT-038 "set interpolation"). Every assistant is one typed command over a key selection that
//! writes ordinary `Keyframe` fields through `Property::set_keyframes` (one revision step, property
//! unchanged on refusal).
//!
//! * Ease / Ease In / Ease Out (STU-MOT-037): bezier with speed 0 and the default influence
//!   0.16666666666 (STU-MOT-034) on both, the incoming or the outgoing side, for every component.
//!   (After Effects' F9 "Easy Ease" assigns 33.33 % influence; the spec states the default influence
//!   for the ease assistants, and Studio follows the spec: AE numeric parity UNVERIFIED, D5.)
//! * Toggle hold: the selected keys' outgoing interpolation becomes `hold`, or, when every selected
//!   key already holds, `linear`.
//! * Auto bezier (STU-MOT-031): both sides `auto_bezier` with stored tangents derived from the
//!   neighbours, speed per component `(v[i+1] - v[i-1]) / (t[i+1] - t[i-1])` (seconds), one-sided
//!   chord slope at the first and last key, 0 for a lone key, influence 0.16666666666. Derivation
//!   is an authoring-time operation (the evaluator never derives); re-run after keys move. Adobe's
//!   own auto-bezier derivation is UNVERIFIED (D5).
//! * Continuous bezier: both sides `continuous_bezier`; the INCOMING speed wins and the outgoing
//!   speed is set equal to it (influences kept). Oracle (D5, After Effects 26.5
//!   `setTemporalContinuousAtKey`, corpus `adobe-session/aftereffects/AE-01b__mixed_types.json` row
//!   `chain3 continuous`): ease in (20, 40 %) / out (60, 25 %) reads back as in (20, 40 %) / out
//!   (20, 25 %) and all 101 samples match to 1e-9 (test `d5_ae_oracle::ae01b_mixed_types_and_chains`).
//!   One oracle key only: a rule other than "incoming wins" that also yields 20 here (for example the
//!   smaller speed) is not excluded (UNVERIFIED beyond this case).
//! * Roving (STU-MOT-035): roving keys get new times so that speed is constant between the fixed
//!   keys around them: times proportional to the cumulative straight-line distance between
//!   successive key values (all components), or the arc length along the spatial Bezier when the
//!   property has spatial tangents (`crate::spatial`, D5 oracle AE-02 `three_keys_roving_middle`:
//!   rove time 1.376821 s vs After Effects 1.376790 s, which AE stores quantised to 1/1024 frame),
//!   rounded half up to whole ticks; a zero-length run spaces the roving keys evenly. The first
//!   and last keys may never rove (`roving_endpoint`); a retiming that would put two keys on one tick
//!   is `roving_collapse`.
//! * Sequence layers (After Effects keyframe assistant; NO spec clause yet, Studio extension, AE
//!   parity UNVERIFIED): layers are placed end to end in the given order starting at the first
//!   layer's in point, each starting `overlap` ticks before the previous one ends; the optional
//!   transition writes linear opacity keyframes (percent) over the overlap: `DissolveFront` fades the
//!   earlier layer out, `CrossDissolve` also fades the later one in.
use crate::{
    error::MotionError,
    keyframe::{DEFAULT_INFLUENCE, Interpolation, Keyframe, TemporalTangent},
    property::Property,
};
use hsk_studio_accord::{TICKS_PER_SECOND, Ticks};

/// Most layers one sequence-layers command accepts.
pub const MAX_SEQUENCE_LAYERS: usize = 4096;

/// Which side(s) an ease assistant writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EaseSide {
    Both,
    In,
    Out,
}

fn check_selection(property: &Property, selection: &[usize]) -> Result<(), MotionError> {
    let n = property.keyframes().len();
    if selection.iter().any(|i| *i >= n) {
        return Err(MotionError::SelectionOutOfRange);
    }
    Ok(())
}

/// Ease / Ease In / Ease Out on the selected keys (module docs).
pub fn ease(property: &mut Property, selection: &[usize], side: EaseSide) -> Result<(), MotionError> {
    check_selection(property, selection)?;
    let mut keys = property.keyframes().to_vec();
    let ease = vec![TemporalTangent::EASE];
    for &i in selection {
        let k = &mut keys[i];
        if matches!(side, EaseSide::Both | EaseSide::In) {
            k.in_interp = Interpolation::Bezier;
            k.in_tangent = ease.clone();
        }
        if matches!(side, EaseSide::Both | EaseSide::Out) {
            k.out_interp = Interpolation::Bezier;
            k.out_tangent = ease.clone();
        }
    }
    property.set_keyframes(keys)
}

/// Toggle hold on the selected keys' outgoing interpolation (module docs).
pub fn toggle_hold(property: &mut Property, selection: &[usize]) -> Result<(), MotionError> {
    check_selection(property, selection)?;
    let mut keys = property.keyframes().to_vec();
    let all_hold = selection.iter().all(|i| keys[*i].out_interp == Interpolation::Hold);
    for &i in selection {
        let k = &mut keys[i];
        if all_hold {
            k.out_interp = Interpolation::Linear;
            k.out_tangent.clear();
        } else {
            k.out_interp = Interpolation::Hold;
        }
    }
    property.set_keyframes(keys)
}

/// Sets one interpolation type on a side of the selected keys (STU-MOT-038). Setting a bezier
/// family type on a side without tangents stores the default ease tangent; auto bezier derives its
/// tangents ([`auto_bezier`]).
pub fn set_interpolation(
    property: &mut Property,
    selection: &[usize],
    side: EaseSide,
    interp: Interpolation,
) -> Result<(), MotionError> {
    check_selection(property, selection)?;
    let mut keys = property.keyframes().to_vec();
    for &i in selection {
        let k = &mut keys[i];
        for (write, kind, tangents) in [
            (matches!(side, EaseSide::Both | EaseSide::In), &mut k.in_interp, &mut k.in_tangent),
            (matches!(side, EaseSide::Both | EaseSide::Out), &mut k.out_interp, &mut k.out_tangent),
        ] {
            if !write {
                continue;
            }
            *kind = interp;
            if interp.is_bezier() && tangents.is_empty() {
                tangents.push(TemporalTangent::EASE);
            }
        }
    }
    property.set_keyframes(keys)?;
    if interp == Interpolation::AutoBezier {
        auto_bezier(property, selection)?;
    }
    Ok(())
}

fn seconds(a: Ticks, b: Ticks) -> f64 {
    (b.value() as f64 - a.value() as f64) / TICKS_PER_SECOND as f64
}

/// Auto bezier on the selected keys (module docs).
pub fn auto_bezier(property: &mut Property, selection: &[usize]) -> Result<(), MotionError> {
    check_selection(property, selection)?;
    let mut keys = property.keyframes().to_vec();
    let n = keys.len();
    let dims = property.dimension();
    for &i in selection {
        let (lo, hi) = (i.saturating_sub(1), (i + 1).min(n - 1));
        let tangents: Vec<TemporalTangent> = (0..dims)
            .map(|c| {
                let speed = if lo == hi {
                    0.0
                } else {
                    (keys[hi].value[c] - keys[lo].value[c]) / seconds(keys[lo].time, keys[hi].time)
                };
                TemporalTangent { speed, influence: DEFAULT_INFLUENCE }
            })
            .collect();
        let k = &mut keys[i];
        k.in_interp = Interpolation::AutoBezier;
        k.out_interp = Interpolation::AutoBezier;
        k.in_tangent = tangents.clone();
        k.out_tangent = tangents;
    }
    property.set_keyframes(keys)
}

/// Continuous bezier on the selected keys (module docs).
pub fn make_continuous(property: &mut Property, selection: &[usize]) -> Result<(), MotionError> {
    check_selection(property, selection)?;
    let record: Vec<crate::graph::VelocityRecord> = selection
        .iter()
        .map(|i| crate::graph::velocity_record(property, *i))
        .collect::<Result<_, _>>()?;
    let mut keys = property.keyframes().to_vec();
    for (&i, r) in selection.iter().zip(&record) {
        let k = &mut keys[i];
        k.in_interp = Interpolation::ContinuousBezier;
        k.out_interp = Interpolation::ContinuousBezier;
        k.in_tangent = r.incoming.clone();
        k.out_tangent = r
            .outgoing
            .iter()
            .zip(&r.incoming)
            .map(|(out, inc)| TemporalTangent { speed: inc.speed, influence: out.influence })
            .collect();
    }
    property.set_keyframes(keys)
}

fn distance(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (y - x) * (y - x)).sum::<f64>().sqrt()
}

/// Roving retiming (module docs). `roving[i]` marks key `i` as roving; length must equal the key
/// count. Returns the new key times (also written to the property).
pub fn rove(property: &mut Property, roving: &[bool]) -> Result<Vec<Ticks>, MotionError> {
    let mut keys = property.keyframes().to_vec();
    let n = keys.len();
    if roving.len() != n {
        return Err(MotionError::SelectionOutOfRange);
    }
    if n > 0 && (roving[0] || roving[n - 1]) {
        return Err(MotionError::RovingEndpoint);
    }
    let mut a = 0usize;
    while a + 1 < n {
        let b = (a + 1..n).find(|j| !roving[*j]).unwrap_or(n - 1);
        if b > a + 1 {
            let mut cumulative = Vec::with_capacity(b - a);
            let mut total = 0.0;
            for j in a + 1..=b {
                total += match property.spatial_tangents() {
                    Some(t) => crate::spatial::SpatialCurve::new(
                        &keys[j - 1].value,
                        &t[j - 1].out_tangent,
                        &t[j].in_tangent,
                        &keys[j].value,
                    )
                    .length(),
                    None => distance(&keys[j - 1].value, &keys[j].value),
                };
                cumulative.push(total);
            }
            if !total.is_finite() {
                return Err(MotionError::NonFinite);
            }
            let (t0, t1) = (keys[a].time.value(), keys[b].time.value());
            let span = u128::from(t1 - t0);
            for j in a + 1..b {
                let fraction = if total > 0.0 {
                    cumulative[j - a - 1] / total
                } else {
                    (j - a) as f64 / (b - a) as f64
                };
                // Round half up on the exact span fraction (f64 fraction, i128 placement).
                let offset = (fraction * span as f64 + 0.5).floor();
                if !(0.0..=span as f64).contains(&offset) {
                    return Err(MotionError::TickOverflow);
                }
                keys[j].time = Ticks::new(t0 + offset as u64);
            }
        }
        a = b;
    }
    if keys.windows(2).any(|w| w[0].time >= w[1].time) {
        return Err(MotionError::RovingCollapse);
    }
    let times = keys.iter().map(|k| k.time).collect();
    property.set_keyframes(keys)?;
    Ok(times)
}

/// The in/out span of a layer to sequence (ticks; `out > in`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayerSpan {
    pub in_tick: u64,
    pub out_tick: u64,
}

/// Transition written by [`sequence_layers`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceTransition {
    Off,
    DissolveFront,
    CrossDissolve,
}

/// One sequenced layer: its new span and its opacity keyframes (percent; empty = untouched).
#[derive(Clone, Debug, PartialEq)]
pub struct SequencedLayer {
    pub span: LayerSpan,
    pub opacity: Vec<Keyframe>,
}

/// Sequence layers (module docs). `overlap` must be shorter than every layer.
pub fn sequence_layers(
    layers: &[LayerSpan],
    overlap: u64,
    transition: SequenceTransition,
) -> Result<Vec<SequencedLayer>, MotionError> {
    if layers.len() > MAX_SEQUENCE_LAYERS {
        return Err(MotionError::CountLimit { what: "sequence_layers" });
    }
    if layers.iter().any(|l| l.out_tick <= l.in_tick) {
        return Err(MotionError::InvalidBounds);
    }
    if layers.iter().any(|l| overlap >= l.out_tick - l.in_tick) {
        return Err(MotionError::InvalidParameter { parameter: "overlap" });
    }
    if transition != SequenceTransition::Off && overlap == 0 {
        return Err(MotionError::InvalidParameter { parameter: "overlap" });
    }
    let mut out = Vec::with_capacity(layers.len());
    let mut start = layers.first().map_or(0, |l| l.in_tick);
    for (i, l) in layers.iter().enumerate() {
        let duration = l.out_tick - l.in_tick;
        let end = start.checked_add(duration).ok_or(MotionError::TickOverflow)?;
        let mut opacity = Vec::new();
        let fades_in = transition == SequenceTransition::CrossDissolve && i > 0;
        let fades_out = transition != SequenceTransition::Off && i + 1 < layers.len();
        if fades_in {
            opacity.push(Keyframe::new(Ticks::new(start), vec![0.0]));
            opacity.push(Keyframe::new(Ticks::new(start + overlap), vec![100.0]));
        }
        if fades_in && fades_out && overlap.saturating_mul(2) > duration {
            return Err(MotionError::InvalidParameter { parameter: "overlap" });
        }
        if fades_out {
            if !fades_in || end - overlap > start + overlap {
                opacity.push(Keyframe::new(Ticks::new(end - overlap), vec![100.0]));
            }
            opacity.push(Keyframe::new(Ticks::new(end), vec![0.0]));
        }
        out.push(SequencedLayer { span: LayerSpan { in_tick: start, out_tick: end }, opacity });
        start = end - overlap;
    }
    Ok(out)
}
