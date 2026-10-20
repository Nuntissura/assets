//! Lane SMB-AE-MOTION-TOOLS slice 1: graph editor model (value and speed graphs, separated
//! dimensions, velocity record) and keyframe assistants (ease, hold, auto bezier, continuity,
//! roving, sequence layers). Expected values are hand-computed in the comments, independent of the
//! implementation. After Effects numeric parity is UNVERIFIED (D5).
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_motion::*;

const S: i64 = 254_016_000_000;

fn key(time: i64, value: &[f64]) -> Keyframe {
    Keyframe::new(Ticks::new(time as u64), value.to_vec())
}

fn at(p: &Property, tick: i64) -> Vec<f64> {
    p.sample(EvalTick::new(tick), &CancellationToken::default()).unwrap()
}

fn close(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol, "{a} != {b} (tol {tol})");
}

#[test]
fn speed_and_value_graphs_match_hand_computed_curves() {
    // Speed 0, influence 1/3 on both sides: X = [0, 1/3, 2/3, 1] so x(s) = s, Y = [0, 0, 100, 100]
    // so v(u) = 100 (3u^2 - 2u^3) and dv/dt = 100 (6u - 6u^2) per second over a 1 s segment.
    let third = TemporalTangent::new(0.0, 1.0 / 3.0).unwrap();
    let k0 = key(0, &[0.0]).with_interpolation(Interpolation::Linear, Interpolation::Bezier).with_tangents(vec![], vec![third]);
    let k1 = key(S, &[100.0]).with_interpolation(Interpolation::Bezier, Interpolation::Linear).with_tangents(vec![third], vec![]);
    let ease = Property::new("x", vec![0.0], vec![k0, k1]).unwrap();
    close(at(&ease, S / 4)[0], 15.625, 1e-9);
    close(speed_at(&ease, 0, EvalTick::new(S / 4)).unwrap(), 112.5, 1e-6);
    close(at(&ease, S / 2)[0], 50.0, 1e-9);
    close(speed_at(&ease, 0, EvalTick::new(S / 2)).unwrap(), 150.0, 1e-6);
    close(speed_at(&ease, 0, EvalTick::new(0)).unwrap(), 0.0, 1e-9);
    let cancel = CancellationToken::default();
    let values = graph(&ease, GraphKind::Value, GraphChannel::Component(0), EvalTick::new(0), EvalTick::new(S), 3, &cancel).unwrap();
    assert_eq!(values.iter().map(|p| p.tick).collect::<Vec<_>>(), vec![0, S / 2, S]);
    close(values[1].y, 50.0, 1e-9);
    close(values[2].y, 100.0, 0.0);

    // Linear 2D (0,0) -> (60,80) over 2 s: component speeds 30 and 40, path speed 50; held after the
    // last key (speed 0).
    let lin = Property::new("pos", vec![0.0, 0.0], vec![key(0, &[0.0, 0.0]), key(2 * S, &[60.0, 80.0])]).unwrap();
    close(speed_at(&lin, 1, EvalTick::new(S / 3)).unwrap(), 40.0, 1e-9);
    let speed = graph(&lin, GraphKind::Speed, GraphChannel::Magnitude, EvalTick::new(0), EvalTick::new(2 * S), 5, &cancel).unwrap();
    assert_eq!(speed.iter().map(|p| p.tick).collect::<Vec<_>>(), vec![0, S / 2, S, 3 * S / 2, 2 * S]);
    for p in &speed[..4] {
        close(p.y, 50.0, 1e-9);
    }
    assert_eq!(speed[4].y, 0.0);
    assert_eq!(fit_range(&[&speed]), Some((0.0, 50.0)));

    // Hold segment: speed 0 in the middle.
    let hold = Property::new("h", vec![0.0], vec![key(0, &[0.0]).with_interpolation(Interpolation::Linear, Interpolation::Hold), key(S, &[9.0])]).unwrap();
    assert_eq!(speed_at(&hold, 0, EvalTick::new(S / 2)).unwrap(), 0.0);
}

#[test]
fn ease_hold_auto_bezier_and_velocity_record() {
    let keys = vec![key(0, &[0.0]), key(S, &[10.0]), key(2 * S, &[30.0])];
    let mut p = Property::new("x", vec![0.0], keys).unwrap();
    let r0 = p.revision();
    ease(&mut p, &[1], EaseSide::Both).unwrap();
    let k1 = &p.keyframes()[1];
    assert_eq!((k1.in_interp, k1.out_interp), (Interpolation::Bezier, Interpolation::Bezier));
    assert_eq!(k1.out_tangent, vec![TemporalTangent { speed: 0.0, influence: 0.16666666666 }]);
    assert_eq!(speed_at(&p, 0, EvalTick::new(S)).unwrap(), 0.0);
    assert_eq!(p.revision(), r0 + 1);
    ease(&mut p, &[0], EaseSide::Out).unwrap();
    assert_eq!((p.keyframes()[0].in_interp, p.keyframes()[0].out_interp), (Interpolation::Linear, Interpolation::Bezier));

    toggle_hold(&mut p, &[0, 1]).unwrap();
    assert!(p.keyframes()[..2].iter().all(|k| k.out_interp == Interpolation::Hold));
    toggle_hold(&mut p, &[0, 1]).unwrap();
    assert!(p.keyframes()[..2].iter().all(|k| k.out_interp == Interpolation::Linear));

    // Auto bezier speeds: first key chord 10/1 s = 10, middle (30 - 0) / 2 s = 15, last chord 20/1 s.
    auto_bezier(&mut p, &[0, 1, 2]).unwrap();
    let speeds: Vec<f64> = p.keyframes().iter().map(|k| k.out_tangent[0].speed).collect();
    assert_eq!(speeds, vec![10.0, 15.0, 20.0]);
    close(speed_at(&p, 0, EvalTick::new(0)).unwrap(), 10.0, 1e-9);
    close(speed_at(&p, 0, EvalTick::new(S)).unwrap(), 15.0, 1e-9);
    let record = velocity_record(&p, 1).unwrap();
    assert!(record.continuous);
    close(record.incoming[0].speed, 15.0, 0.0);

    // A continuity record with unequal speeds is refused, never rewritten; the property is unchanged.
    let before = p.clone();
    let bad = VelocityRecord {
        incoming: vec![TemporalTangent { speed: 1.0, influence: 0.5 }],
        outgoing: vec![TemporalTangent { speed: 2.0, influence: 0.5 }],
        continuous: true,
    };
    assert_eq!(set_velocity(&mut p, 1, &bad), Err(MotionError::InvalidParameter { parameter: "continuity_speed" }));
    assert_eq!(p.keyframes(), before.keyframes());
    let split = VelocityRecord { continuous: false, ..bad };
    set_velocity(&mut p, 1, &split).unwrap();
    assert_eq!(p.keyframes()[1].in_interp, Interpolation::Bezier);
    close(speed_at(&p, 0, EvalTick::new(S)).unwrap(), 2.0, 1e-9);
}

#[test]
fn roving_and_sequence_layers_retime_by_hand_computed_ticks() {
    // Path (0,0) -> (30,40) -> (30,140): legs 50 and 100 over 3 s => the roving middle key lands at
    // 3 s * 50 / 150 = 1 s, and both legs then run at 50 units/s.
    let keys = vec![key(0, &[0.0, 0.0]), key(S / 10, &[30.0, 40.0]), key(3 * S, &[30.0, 140.0])];
    let mut p = Property::new("pos", vec![0.0, 0.0], keys).unwrap();
    let times = rove(&mut p, &[false, true, false]).unwrap();
    assert_eq!(times, vec![Ticks::new(0), Ticks::new(S as u64), Ticks::new(3 * S as u64)]);
    close(path_speed_at(&p, EvalTick::new(S / 2)).unwrap(), 50.0, 1e-9);
    close(path_speed_at(&p, EvalTick::new(2 * S)).unwrap(), 50.0, 1e-9);
    assert_eq!(rove(&mut p, &[true, false, false]), Err(MotionError::RovingEndpoint));

    // Durations 2 s, 3 s, 1 s, overlap 0.5 s: spans [0, 2], [1.5, 4.5], [4, 5] s.
    let h = S as u64 / 2;
    let s = S as u64;
    let layers = [
        LayerSpan { in_tick: 0, out_tick: 2 * s },
        LayerSpan { in_tick: 5 * s, out_tick: 8 * s },
        LayerSpan { in_tick: s, out_tick: 2 * s },
    ];
    let seq = sequence_layers(&layers, h, SequenceTransition::CrossDissolve).unwrap();
    let spans: Vec<(u64, u64)> = seq.iter().map(|l| (l.span.in_tick, l.span.out_tick)).collect();
    assert_eq!(spans, vec![(0, 4 * h), (3 * h, 9 * h), (8 * h, 10 * h)]);
    let keys = |i: usize| seq[i].opacity.iter().map(|k| (k.time.value(), k.value[0])).collect::<Vec<_>>();
    assert_eq!(keys(0), vec![(3 * h, 100.0), (4 * h, 0.0)]);
    assert_eq!(keys(1), vec![(3 * h, 0.0), (4 * h, 100.0), (8 * h, 100.0), (9 * h, 0.0)]);
    assert_eq!(keys(2), vec![(8 * h, 0.0), (9 * h, 100.0)]);
    let off = sequence_layers(&layers, 0, SequenceTransition::Off).unwrap();
    assert_eq!(off[2].span, LayerSpan { in_tick: 5 * s, out_tick: 6 * s });
    assert!(off.iter().all(|l| l.opacity.is_empty()));
}

#[test]
fn separate_and_join_dimensions_are_stated_conversions() {
    let out = vec![TemporalTangent::new(5.0, 0.2).unwrap(), TemporalTangent::new(-3.0, 0.4).unwrap()];
    let arrive = vec![TemporalTangent::new(0.0, 0.5).unwrap(), TemporalTangent::new(1.0, 0.25).unwrap()];
    let k0 = key(0, &[0.0, 10.0]).with_interpolation(Interpolation::Linear, Interpolation::Bezier).with_tangents(vec![], out);
    let k1 = key(S, &[100.0, 0.0]).with_interpolation(Interpolation::Bezier, Interpolation::Linear).with_tangents(arrive, vec![]);
    let p = Property::new("pos", vec![0.0, 0.0], vec![k0, k1]).unwrap();
    let parts = separate_dimensions(&p).unwrap();
    assert_eq!(parts.iter().map(|q| q.key().to_owned()).collect::<Vec<_>>(), vec!["pos_0", "pos_1"]);
    let joined = join_dimensions("pos", &parts).unwrap();
    for t in [0, S / 7, S / 3, S / 2, 5 * S / 6, S] {
        let want = at(&p, t);
        assert_eq!(at(&parts[0], t)[0], want[0], "separate is exact (component 0, tick {t})");
        assert_eq!(at(&parts[1], t)[0], want[1], "separate is exact (component 1, tick {t})");
        assert_eq!(at(&joined, t), want, "join of an exact separation round-trips (tick {t})");
    }

    // Differing key times: A keys at 0 s and 1 s, B at 0 s and 2 s (both linear). The union is
    // 0, 1, 2 s; B gains a key at 1 s carrying its sampled value 10, A one at 2 s holding 10.
    let a = Property::new("a", vec![0.0], vec![key(0, &[0.0]), key(S, &[10.0])]).unwrap();
    let b = Property::new("b", vec![0.0], vec![key(0, &[0.0]), key(2 * S, &[20.0])]).unwrap();
    let j = join_dimensions("ab", &[a, b]).unwrap();
    assert_eq!(j.keyframes().len(), 3);
    assert_eq!(at(&j, 0), vec![0.0, 0.0]);
    assert_eq!(at(&j, S), vec![10.0, 10.0]);
    assert_eq!(at(&j, 2 * S), vec![10.0, 20.0]);
    assert_eq!(j.keyframes()[1].in_interp, Interpolation::Bezier);
}

#[test]
fn hostile_inputs_are_refused_before_allocation() {
    let p = Property::new("x", vec![0.0], vec![key(0, &[0.0]), key(S, &[1.0])]).unwrap();
    let cancel = CancellationToken::default();
    for samples in [0, 1, MAX_GRAPH_SAMPLES + 1, usize::MAX] {
        assert_eq!(
            graph(&p, GraphKind::Speed, GraphChannel::Component(0), EvalTick::new(0), EvalTick::new(S), samples, &cancel),
            Err(MotionError::SampleLimit),
            "samples {samples}"
        );
    }
    assert_eq!(
        graph(&p, GraphKind::Value, GraphChannel::Component(0), EvalTick::new(S), EvalTick::new(0), 2, &cancel),
        Err(MotionError::InvalidBounds)
    );
    assert_eq!(speed_at(&p, 3, EvalTick::new(0)), Err(MotionError::ComponentMismatch));
    // Key bomb.
    let bomb: Vec<Keyframe> = (0..=MAX_KEYFRAMES as i64).map(|i| key(i, &[0.0])).collect();
    assert_eq!(Property::new("x", vec![0.0], bomb).err(), Some(MotionError::KeyframeLimit));
    // Selection outside the keys: refused, property and revision unchanged.
    let mut q = p.clone();
    assert_eq!(ease(&mut q, &[0, 5], EaseSide::Both), Err(MotionError::SelectionOutOfRange));
    assert_eq!((q.revision(), q.keyframes()), (p.revision(), p.keyframes()));
    // Too many layers / overlap not shorter than a layer / too many joined components.
    let many = vec![LayerSpan { in_tick: 0, out_tick: 10 }; MAX_SEQUENCE_LAYERS + 1];
    assert_eq!(sequence_layers(&many, 0, SequenceTransition::Off), Err(MotionError::CountLimit { what: "sequence_layers" }));
    assert_eq!(
        sequence_layers(&[LayerSpan { in_tick: 0, out_tick: 10 }], 10, SequenceTransition::DissolveFront),
        Err(MotionError::InvalidParameter { parameter: "overlap" })
    );
    let parts = vec![p.clone(); MAX_DIMENSION + 1];
    assert_eq!(join_dimensions("x", &parts).err(), Some(MotionError::DimensionLimit));
}

/// Lane SMB-AI-AE-ORACLE-FOLLOWUPS slice 3: spatial tangents (hand values). (1) Zero tangents on
/// (0,0) -> (30,40) over 2 s, linear: the 50 px path is travelled uniformly, t = 0.5 s -> 12.5 px ->
/// (7.5, 10). (2) Symmetric bump P0 = (0,0), out0 = (0,8), in1 = (0,8), P3 = (20,0): the curve is
/// mirror-symmetric about x = 10, so half the arc length (t = 1 s of 2 s, linear) is the apex
/// B(1/2) = (P0 + 3 P1 + 3 P2 + P3) / 8 = (10, 6). (3) Spatial auto-bezier on (0,0), (10,0), (10,10):
/// interior out = ((10,10) - (0,0)) / 6, in = -out, ends zero. (4) Roving a straight 3-key path with
/// zero tangents: 50 + 50 px, the middle key lands half way (2 s of 0..4 s). Hostile inputs are
/// typed refusals that leave the property unchanged.
#[test]
fn spatial_tangents_arc_length_auto_bezier_and_refusals() {
    let pos = |keys: Vec<Keyframe>| Property::new("position", vec![0.0, 0.0], keys).unwrap();
    let base = pos(vec![key(0, &[0.0, 0.0]), key(2 * S, &[30.0, 40.0])]);
    let straight = base.clone().with_spatial_tangents(vec![SpatialTangents::zero(2); 2]).unwrap();
    let v = at(&straight, S / 2);
    close(v[0], 7.5, 1e-9);
    close(v[1], 10.0, 1e-9);
    let bump = pos(vec![key(0, &[0.0, 0.0]), key(2 * S, &[20.0, 0.0])])
        .with_spatial_tangents(vec![
            SpatialTangents { in_tangent: vec![0.0, 0.0], out_tangent: vec![0.0, 8.0] },
            SpatialTangents { in_tangent: vec![0.0, 8.0], out_tangent: vec![0.0, 0.0] },
        ])
        .unwrap();
    let v = at(&bump, S);
    close(v[0], 10.0, 1e-9);
    close(v[1], 6.0, 1e-9);
    assert_eq!(at(&bump, 3 * S), vec![20.0, 0.0]);
    let mut auto = pos(vec![key(0, &[0.0, 0.0]), key(S, &[10.0, 0.0]), key(2 * S, &[10.0, 10.0])]);
    spatial_auto_bezier(&mut auto, &[0, 1, 2]).unwrap();
    let t = auto.spatial_tangents().unwrap();
    assert_eq!((&t[0], &t[2]), (&SpatialTangents::zero(2), &SpatialTangents::zero(2)));
    assert_eq!(t[1].out_tangent, vec![10.0 / 6.0, 10.0 / 6.0]);
    assert_eq!(t[1].in_tangent, vec![-10.0 / 6.0, -10.0 / 6.0]);
    let mut roving = pos(vec![key(0, &[0.0, 0.0]), key(S, &[30.0, 40.0]), key(4 * S, &[60.0, 80.0])])
        .with_spatial_tangents(vec![SpatialTangents::zero(2); 3])
        .unwrap();
    assert_eq!(rove(&mut roving, &[false, true, false]).unwrap()[1], Ticks::new((2 * S) as u64));
    assert_eq!(base.clone().with_spatial_tangents(vec![SpatialTangents::zero(2)]).err(), Some(MotionError::ComponentMismatch));
    assert_eq!(
        base.clone().with_spatial_tangents(vec![SpatialTangents::zero(2), SpatialTangents::zero(3)]).err(),
        Some(MotionError::ComponentMismatch)
    );
    let mut nan = vec![SpatialTangents::zero(2); 2];
    nan[1].in_tangent[1] = f64::NAN;
    assert_eq!(base.clone().with_spatial_tangents(nan).err(), Some(MotionError::NonFinite));
    let scalar = Property::new("x", vec![0.0], vec![key(0, &[0.0])]).unwrap();
    assert_eq!(scalar.with_spatial_tangents(vec![SpatialTangents::zero(1)]).err(), Some(MotionError::ComponentMismatch));
    let mut kept = straight.clone();
    let revision = kept.revision();
    assert_eq!(kept.set_spatial_tangents(Some(Vec::new())).err(), Some(MotionError::ComponentMismatch));
    assert_eq!(spatial_auto_bezier(&mut kept, &[5]).err(), Some(MotionError::SelectionOutOfRange));
    assert_eq!((kept.revision(), kept.spatial_tangents().map(<[_]>::len)), (revision, Some(2)));
}
