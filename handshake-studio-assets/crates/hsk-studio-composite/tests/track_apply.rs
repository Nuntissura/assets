//! Track apply and stabilise (lane SMB-AE-KEY-TRACK slice 3). Synthetic tracks with exact known
//! motion, plus one end-to-end run through pigment's tracker on rendered frames. The landing point
//! of a layer pixel is computed with the composition adapter's transform convention
//! (`position + R(rotation) S(scale) (x - anchor)`, pigment `Affine2::from_parts`).
#![allow(clippy::needless_range_loop)]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::track_apply::*;
use hsk_studio_folio::{
    StudioCompositionKeyframe, StudioCompositionLayer, StudioInterpolation, StudioLayerProperty, StudioLayerTiming,
    StudioPropertyStream, StudioRational,
};
use hsk_studio_pigment::track_math::*;
use hsk_studio_pigment::transform_math::Affine2;

const TIMING: TrackTiming = TrackTiming { first_tick: 1_000, ticks_per_frame: 100 };

fn layer(streams: Vec<StudioPropertyStream>) -> StudioCompositionLayer {
    StudioCompositionLayer {
        layer_id: "SLYR-footage".to_owned(),
        timing: StudioLayerTiming {
            in_point: 0,
            out_point: 100_000,
            start_time: 0,
            stretch: StudioRational { numerator: 1, denominator: 1 },
        },
        parent: None,
        precomp: None,
        track_matte: None,
        streams,
    }
}

fn sample(frame: usize, p: [f64; 2]) -> TrackSample {
    TrackSample { frame, position: p, confidence: 1.0, status: TrackStatus::Tracked }
}

/// Exact two-point motion about (64, 64): rotation, scale and translation per frame.
fn moved(t: f64, p: [f64; 2]) -> [f64; 2] {
    let (th, s) = ((3.0 * t - 0.2 * t * t).to_radians(), 1.0 + 0.02 * t);
    let (dx, dy) = (p[0] - 64.0, p[1] - 64.0);
    [64.0 + 2.0 * t + s * (th.cos() * dx - th.sin() * dy), 64.0 - t + s * (th.sin() * dx + th.cos() * dy)]
}

const A0: [f64; 2] = [40.0, 50.0];
const B0: [f64; 2] = [80.0, 60.0];

fn two_point_track() -> (Vec<TrackSample>, Vec<TrackSample>, MotionTrack) {
    let a: Vec<TrackSample> = (0..11).map(|f| sample(f, moved(f as f64, A0))).collect();
    let b: Vec<TrackSample> = (0..11).map(|f| sample(f, moved(f as f64, B0))).collect();
    let solve = solve_two_point(&a, &b, 0).unwrap();
    let track = MotionTrack::from_two_point(&a, &b, &solve).unwrap();
    (a, b, track)
}

fn value(l: &StudioCompositionLayer, p: StudioLayerProperty, tick: u64) -> Option<Vec<f64>> {
    let s = l.stream(p)?;
    Some(s.keyframes.iter().find(|k| k.time == tick).map_or_else(|| s.static_value.clone(), |k| k.value.clone()))
}

/// Where layer point `x` lands at `tick`.
fn landing(l: &StudioCompositionLayer, tick: u64, x: [f64; 2]) -> [f64; 2] {
    let anchor = value(l, StudioLayerProperty::Anchor, tick).unwrap();
    let position = value(l, StudioLayerProperty::Position, tick).unwrap();
    let scale = value(l, StudioLayerProperty::Scale, tick).unwrap_or(vec![100.0, 100.0]);
    let rot = value(l, StudioLayerProperty::Rotation, tick).map_or(0.0, |v| v[0]);
    let m = Affine2::from_parts(anchor[0], anchor[1], scale[0] / 100.0, scale[1] / 100.0, rot, position[0] - anchor[0], position[1] - anchor[1]);
    let (x, y) = m.apply(x[0], x[1]);
    [x, y]
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

#[test]
fn apply_writes_linear_keys_on_the_chosen_properties_only() {
    let (a, _, track) = two_point_track();
    let opacity = StudioPropertyStream {
        property: StudioLayerProperty::Opacity,
        static_value: vec![100.0],
        keyframes: vec![StudioCompositionKeyframe {
            time: 0,
            value: vec![80.0],
            in_interp: StudioInterpolation::Hold,
            out_interp: StudioInterpolation::Hold,
            in_tangent: vec![],
            out_tangent: vec![],
        }],
    };
    let scale = StudioPropertyStream { property: StudioLayerProperty::Scale, static_value: vec![50.0, 50.0], keyframes: vec![] };
    let target = layer(vec![opacity.clone(), scale.clone()]);
    let axes = TrackAxes { position: true, rotation: true, scale: false };
    let out = apply_track(&target, &track, TIMING, axes, [5.0, -5.0], 10.0, [50.0, 50.0]).unwrap();
    assert_eq!(target.streams, vec![opacity.clone(), scale.clone()], "the input record is untouched (history swap)");
    assert_eq!(out.stream(StudioLayerProperty::Opacity), Some(&opacity));
    assert_eq!(out.stream(StudioLayerProperty::Scale), Some(&scale), "an unchosen property is not keyed");
    let pos = out.stream(StudioLayerProperty::Position).unwrap();
    let rot = out.stream(StudioLayerProperty::Rotation).unwrap();
    assert_eq!(pos.keyframes.len(), 11);
    for (f, k) in pos.keyframes.iter().enumerate() {
        assert_eq!(k.time, 1_000 + 100 * f as u64);
        assert_eq!((k.in_interp, k.out_interp), (StudioInterpolation::Linear, StudioInterpolation::Linear));
        assert!(dist([k.value[0], k.value[1]], [a[f].position[0] + 5.0, a[f].position[1] - 5.0]) < 1e-12);
        let t = f as f64;
        assert!((rot.keyframes[f].value[0] - (10.0 + 3.0 * t - 0.2 * t * t)).abs() < 1e-9, "{:?}", rot.keyframes[f]);
    }
    // Scale chosen: keys on the existing stream, its static value kept.
    let out = apply_track(&target, &track, TIMING, TrackAxes { scale: true, ..TrackAxes::default() }, [0.0; 2], 0.0, [50.0, 50.0]).unwrap();
    let s = out.stream(StudioLayerProperty::Scale).unwrap();
    assert_eq!(s.static_value, vec![50.0, 50.0]);
    assert!((s.keyframes[10].value[0] - 50.0 * 1.2).abs() < 1e-9);
    // One-point tracks key only analysed frames.
    let mut one: Vec<TrackSample> = (0..4).map(|f| sample(f, [10.0 + f as f64, 20.0])).collect();
    one[2].status = TrackStatus::Lost;
    one[3].status = TrackStatus::Lost;
    let out = apply_track(&target, &MotionTrack::from_one_point(&one).unwrap(), TIMING, TrackAxes { position: true, ..TrackAxes::default() }, [0.0; 2], 0.0, [100.0, 100.0]).unwrap();
    assert_eq!(out.stream(StudioLayerProperty::Position).unwrap().keyframes.len(), 2);
}

#[test]
fn lock_off_stabilisation_holds_both_tracked_points_still() {
    let (a, b, track) = two_point_track();
    let all = TrackAxes { position: true, rotation: true, scale: true };
    let out = stabilize(&layer(vec![]), &track, TIMING, all, 0, 0).unwrap();
    let mut worst = 0.0f64;
    for f in 0..11 {
        let tick = 1_000 + 100 * f as u64;
        worst = worst.max(dist(landing(&out, tick, a[f].position), A0));
        worst = worst.max(dist(landing(&out, tick, b[f].position), B0));
    }
    assert!(worst < 1e-9, "both features land on their reference positions, worst {worst}");
    // Position only: the anchor pivots on the feature and translation is removed, rotation kept.
    let pos_only = stabilize(&layer(vec![]), &track, TIMING, TrackAxes { position: true, ..TrackAxes::default() }, 0, 0).unwrap();
    assert!(pos_only.stream(StudioLayerProperty::Rotation).is_none());
    for f in 0..11 {
        assert!(dist(landing(&pos_only, 1_000 + 100 * f as u64, a[f].position), A0) < 1e-9);
    }
    // Reference frame 5: everything is held where frame 5 had it.
    let out5 = stabilize(&layer(vec![]), &track, TIMING, all, 5, 0).unwrap();
    for f in 0..11 {
        assert!(dist(landing(&out5, 1_000 + 100 * f as u64, b[f].position), b[5].position) < 1e-9);
    }
}

#[test]
fn smoothing_keeps_the_slow_move_and_removes_the_shake() {
    // A pan of 3 px per frame with an alternating 0.5 px vertical shake.
    let samples: Vec<TrackSample> =
        (0..20).map(|f| sample(f, [20.0 + 3.0 * f as f64, 50.0 + if f % 2 == 0 { 0.5 } else { -0.5 }])).collect();
    let track = MotionTrack::from_one_point(&samples).unwrap();
    let out = stabilize(&layer(vec![]), &track, TIMING, TrackAxes { position: true, ..TrackAxes::default() }, 0, 2).unwrap();
    for f in 2..18 {
        let land = landing(&out, 1_000 + 100 * f as u64, samples[f].position);
        assert!((land[0] - (20.0 + 3.0 * f as f64)).abs() < 1e-9, "the pan is kept: {land:?}");
        assert!((land[1] - 50.0).abs() <= 0.1 + 1e-9, "shake 0.5 reduced to at most 0.1 by the 5-frame mean: {land:?}");
    }
}

fn tex(x: f64, y: f64) -> f64 {
    0.5 + 0.18 * (0.31 * x + 0.17 * y).sin() + 0.14 * (0.13 * x - 0.29 * y + 1.0).sin() + 0.1 * (0.53 * x + 0.41 * y + 2.0).sin()
}

#[test]
fn tracked_footage_stabilises_end_to_end() {
    // Footage drifting with shake; the true feature motion is known per frame.
    let shake = |t: usize| [[0.0, 0.0], [0.8, -0.6], [-0.7, 0.9], [0.5, 0.4], [-0.9, -0.3], [0.2, 0.7], [0.6, -0.8], [-0.4, 0.1]][t];
    let motion = |t: usize| [1.5 * t as f64 + shake(t)[0], 0.7 * t as f64 + shake(t)[1]];
    let frames: Vec<LumaFrame> = (0..8)
        .map(|t| {
            let m = motion(t);
            let mut data = Vec::new();
            for j in 0..96u32 {
                for i in 0..96u32 {
                    data.push(tex(f64::from(i) + 0.5 - m[0], f64::from(j) + 0.5 - m[1]) as f32);
                }
            }
            LumaFrame::new(96, 96, data).unwrap()
        })
        .collect();
    let p0 = [40.0, 40.0];
    let spec = TrackPointSpec { position: p0, feature_radius: 7, search_radius: 6 };
    let tracks = track_points(&frames, 0, TrackDirection::Forward, &[spec], &TrackParams::DEFAULT, &TrackLimits::DEFAULT, &CancellationToken::default(), &mut |_, _| {}).unwrap();
    let track = MotionTrack::from_one_point(&tracks[0]).unwrap();
    assert_eq!(track.frames().len(), 8);
    let out = stabilize(&layer(vec![]), &track, TIMING, TrackAxes { position: true, ..TrackAxes::default() }, 0, 0).unwrap();
    let mut worst = 0.0f64;
    for t in 0..8 {
        let truth = [p0[0] + motion(t)[0], p0[1] + motion(t)[1]];
        worst = worst.max(dist(landing(&out, 1_000 + 100 * t as u64, truth), p0));
    }
    println!("end-to-end stabilisation residual: worst {worst:.5} px");
    assert!(worst < 0.05, "residual bound 0.05 px, measured {worst}");
}

#[test]
fn hostile_tracks_are_refused_typed() {
    let ok = |f: usize| TrackFrame { frame: f, attach: [1.0, 2.0], rotation_deg: 0.0, scale: 1.0 };
    assert_eq!(MotionTrack::new(vec![]).unwrap_err(), TrackApplyError::Parameter);
    assert_eq!(MotionTrack::new(vec![ok(2), ok(1)]).unwrap_err(), TrackApplyError::Parameter, "unsorted");
    assert_eq!(MotionTrack::new(vec![ok(1), ok(1)]).unwrap_err(), TrackApplyError::Parameter, "duplicate frame");
    assert_eq!(MotionTrack::new(vec![TrackFrame { attach: [f64::NAN, 0.0], ..ok(0) }]).unwrap_err(), TrackApplyError::Parameter);
    assert_eq!(MotionTrack::new(vec![TrackFrame { scale: 0.0, ..ok(0) }]).unwrap_err(), TrackApplyError::Parameter);
    assert_eq!(MotionTrack::new((0..=MAX_TRACK_KEYS).map(ok).collect()).unwrap_err(), TrackApplyError::KeyLimit);
    let track = MotionTrack::new(vec![ok(0), ok(5)]).unwrap();
    let pos = TrackAxes { position: true, ..TrackAxes::default() };
    let l = layer(vec![]);
    let far = TrackTiming { first_tick: u64::MAX - 1, ticks_per_frame: 1 };
    assert_eq!(apply_track(&l, &track, far, pos, [0.0; 2], 0.0, [100.0; 2]).unwrap_err(), TrackApplyError::TickOverflow);
    let zero = TrackTiming { first_tick: 0, ticks_per_frame: 0 };
    assert_eq!(apply_track(&l, &track, zero, pos, [0.0; 2], 0.0, [100.0; 2]).unwrap_err(), TrackApplyError::Parameter);
    assert_eq!(apply_track(&l, &track, TIMING, TrackAxes::default(), [0.0; 2], 0.0, [100.0; 2]).unwrap_err(), TrackApplyError::Parameter);
    assert_eq!(apply_track(&l, &track, TIMING, pos, [f64::INFINITY, 0.0], 0.0, [100.0; 2]).unwrap_err(), TrackApplyError::Parameter);
    assert_eq!(stabilize(&l, &track, TIMING, pos, 0, MAX_SMOOTHING_FRAMES + 1).unwrap_err(), TrackApplyError::Parameter);
    assert_eq!(stabilize(&l, &track, TIMING, pos, 3, 0).unwrap_err(), TrackApplyError::Parameter, "reference frame not analysed");
    assert_eq!(TrackApplyError::TickOverflow.code(), "track_apply_tick_overflow");
}
