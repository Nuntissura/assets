//! Tracker command surface (lane SMB-AE-KEY-TRACK-2, STU-CMP-065a): a model tracks a shot by typed
//! commands only, corrects it, nudges points with the 18 recovered MotionTracker commands and hands
//! the result to stabilisation. Synthetic frames with known motion.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::track_apply::{TrackAxes, TrackTiming, stabilize};
use hsk_studio_composite::tracker::*;
use hsk_studio_folio::{StudioCompositionLayer, StudioLayerProperty, StudioLayerTiming, StudioRational};
use hsk_studio_pigment::track_math::*;

fn tex(x: f64, y: f64) -> f64 {
    0.5 + 0.18 * (0.31 * x + 0.17 * y).sin() + 0.14 * (0.13 * x - 0.29 * y + 1.0).sin() + 0.1 * (0.53 * x + 0.41 * y + 2.0).sin()
}
/// Frame `t` of a pan by (1.25, -0.5) per frame.
fn frames(n: usize) -> Vec<LumaFrame> {
    (0..n)
        .map(|t| {
            let mut data = Vec::new();
            for j in 0..64u32 {
                for i in 0..64u32 {
                    data.push(tex(f64::from(i) + 0.5 - 1.25 * t as f64, f64::from(j) + 0.5 + 0.5 * t as f64) as f32);
                }
            }
            LumaFrame::new(64, 64, data).unwrap()
        })
        .collect()
}
fn truth(p: [f64; 2], t: usize) -> [f64; 2] {
    [p[0] + 1.25 * t as f64, p[1] - 0.5 * t as f64]
}
fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn run(s: &mut TrackerSession, c: TrackerCommand, f: &[LumaFrame]) -> Result<TrackerReceipt, TrackerError> {
    s.apply(c, f, &TrackParams::DEFAULT, &TrackLimits::DEFAULT, &CancellationToken::default(), &mut |_, _| {})
}

#[test]
fn a_shot_is_tracked_corrected_and_stabilised_by_typed_commands() {
    let f = frames(10);
    let mut s = TrackerSession::new(10).unwrap();
    let p0 = [24.0, 30.0];
    run(&mut s, TrackerCommand::AddPoint { position: p0, feature_radius: 7, search_radius: 6, attach_offset: [3.0, -2.0] }, &f).unwrap();
    // Bar 2..=8, analyse forward from 2 needs a position at frame 2: place it (manual placement).
    run(&mut s, TrackerCommand::SetCurrentFrame { frame: 8 }, &f).unwrap();
    run(&mut s, TrackerCommand::SetPointsBarOut, &f).unwrap();
    run(&mut s, TrackerCommand::SetCurrentFrame { frame: 2 }, &f).unwrap();
    run(&mut s, TrackerCommand::SetPointsBarIn, &f).unwrap();
    assert_eq!(s.bar(), (2, 8));
    run(&mut s, TrackerCommand::PlacePoint { point: 0, frame: 2, position: truth(p0, 2) }, &f).unwrap();
    let r = run(&mut s, TrackerCommand::Analyze { direction: TrackDirection::Forward }, &f).unwrap();
    assert_eq!((r.command, r.frames_analysed), ("tracker_analyze", 6), "frames 3..=8 written, the bar bounds the run");
    let samples = s.samples(0).unwrap();
    assert_eq!(samples.iter().map(|x| x.frame).collect::<Vec<_>>(), vec![0, 2, 3, 4, 5, 6, 7, 8]);
    let mut worst = 0.0f64;
    for x in samples.iter().filter(|x| x.frame >= 2) {
        worst = worst.max(dist(x.position, truth(p0, x.frame)));
    }
    assert!(worst < 0.05, "tracked within 0.05 px, measured {worst}");
    // Backward from 2 to the bar's in point writes nothing new beyond frame 2 itself.
    let r = run(&mut s, TrackerCommand::Analyze { direction: TrackDirection::Backward }, &f).unwrap();
    assert_eq!(r.frames_analysed, 0);
    // Correction: move frame 5 off by a placement, re-analyse from it, the track is right again.
    run(&mut s, TrackerCommand::PlacePoint { point: 0, frame: 5, position: truth(p0, 5) }, &f).unwrap();
    run(&mut s, TrackerCommand::SetCurrentFrame { frame: 5 }, &f).unwrap();
    run(&mut s, TrackerCommand::Analyze { direction: TrackDirection::Forward }, &f).unwrap();
    let at8 = s.samples(0).unwrap().into_iter().find(|x| x.frame == 8).unwrap();
    assert!(dist(at8.position, truth(p0, 8)) < 0.05);
    // The attach point (feature + offset) stabilises the footage: it lands on its frame-2 place.
    let track = s.motion_track(0).unwrap();
    let layer = StudioCompositionLayer {
        layer_id: "SLYR-footage".to_owned(),
        timing: StudioLayerTiming { in_point: 0, out_point: 10_000, start_time: 0, stretch: StudioRational { numerator: 1, denominator: 1 } },
        parent: None,
        precomp: None,
        track_matte: None,
        streams: vec![],
        corner_pin: None,
    };
    let timing = TrackTiming { first_tick: 0, ticks_per_frame: 100 };
    let out = stabilize(&layer, &track, timing, TrackAxes { position: true, ..TrackAxes::default() }, 2, 0).unwrap();
    let pos = out.stream(StudioLayerProperty::Position).unwrap();
    assert!(pos.keyframes.iter().all(|k| dist([k.value[0], k.value[1]], [truth(p0, 2)[0] + 3.0, truth(p0, 2)[1] - 2.0]) < 1e-9));
}

#[test]
fn the_eighteen_recovered_commands_and_refusals() {
    let f = frames(3);
    let mut s = TrackerSession::new(3).unwrap();
    run(&mut s, TrackerCommand::AddPoint { position: [20.0, 20.0], feature_radius: 4, search_radius: 4, attach_offset: [0.0, 0.0] }, &f).unwrap();
    run(&mut s, TrackerCommand::SetZoom { screen_px_per_layer_px: 2.0 }, &f).unwrap();
    // 16 nudges: each direction, 1 and 10 screen pixels, with and without the attach point. At
    // zoom 2 a screen pixel is half a layer pixel.
    let mut want = [20.0, 20.0];
    let mut offset = [0.0, 0.0];
    let mut keys = std::collections::BTreeSet::new();
    for keep_attach in [false, true] {
        for direction in [Nudge::Up, Nudge::Down, Nudge::Left, Nudge::Right] {
            for step in [NudgeStep::One, NudgeStep::Ten] {
                let r = run(&mut s, TrackerCommand::MoveTrackPoint { point: 0, direction, step, keep_attach }, &f).unwrap();
                keys.insert(r.command);
                let d = if step == NudgeStep::One { 0.5 } else { 5.0 };
                let (dx, dy) = match direction {
                    Nudge::Up => (0.0, -d),
                    Nudge::Down => (0.0, d),
                    Nudge::Left => (-d, 0.0),
                    Nudge::Right => (d, 0.0),
                };
                want = [want[0] + dx, want[1] + dy];
                if keep_attach {
                    offset = [offset[0] - dx, offset[1] - dy];
                }
                let p = &s.points()[0];
                assert_eq!(p.samples[0].unwrap().position, want);
                assert_eq!(p.attach_offset, offset);
            }
        }
    }
    assert_eq!(keys.len(), 2);
    // A Down-then-Up pair cancels; the attach point stays put under the Not-Attach form.
    let before = s.points()[0].clone();
    run(&mut s, TrackerCommand::MoveTrackPoint { point: 0, direction: Nudge::Down, step: NudgeStep::Ten, keep_attach: true }, &f).unwrap();
    let mid = s.points()[0].clone();
    let attach = |p: &SessionPoint| [p.samples[0].unwrap().position[0] + p.attach_offset[0], p.samples[0].unwrap().position[1] + p.attach_offset[1]];
    assert_eq!(attach(&mid), attach(&before));
    run(&mut s, TrackerCommand::MoveTrackPoint { point: 0, direction: Nudge::Up, step: NudgeStep::Ten, keep_attach: true }, &f).unwrap();
    assert_eq!(s.points()[0], before);
    // The 2 bar commands, and their order rule.
    run(&mut s, TrackerCommand::SetCurrentFrame { frame: 1 }, &f).unwrap();
    assert_eq!(run(&mut s, TrackerCommand::SetPointsBarIn, &f).unwrap().command, "tracker_set_points_bar_in");
    assert_eq!(run(&mut s, TrackerCommand::SetPointsBarOut, &f).unwrap().command, "tracker_set_points_bar_out");
    run(&mut s, TrackerCommand::SetCurrentFrame { frame: 2 }, &f).unwrap();
    assert_eq!(run(&mut s, TrackerCommand::SetPointsBarIn, &f).unwrap_err(), TrackerError::BarOrder);
    // Refusals change nothing.
    let snapshot = s.clone();
    let bad = [
        TrackerCommand::MoveTrackPoint { point: 0, direction: Nudge::Up, step: NudgeStep::One, keep_attach: false },
        TrackerCommand::MoveTrackPoint { point: 7, direction: Nudge::Up, step: NudgeStep::One, keep_attach: false },
        TrackerCommand::SetCurrentFrame { frame: 3 },
        TrackerCommand::SetZoom { screen_px_per_layer_px: f64::NAN },
        TrackerCommand::SetZoom { screen_px_per_layer_px: 1000.0 },
        TrackerCommand::PlacePoint { point: 0, frame: 9, position: [1.0, 1.0] },
        TrackerCommand::PlacePoint { point: 0, frame: 1, position: [f64::INFINITY, 1.0] },
        TrackerCommand::AddPoint { position: [1.0, 1.0], feature_radius: 0, search_radius: 4, attach_offset: [0.0; 2] },
        TrackerCommand::AddPoint { position: [1.0, 1.0], feature_radius: 4, search_radius: 1_000_000, attach_offset: [0.0; 2] },
        TrackerCommand::RemovePoint { point: 3 },
        TrackerCommand::Analyze { direction: TrackDirection::Forward },
    ];
    for c in bad {
        assert_eq!(run(&mut s, c, &f).unwrap_err(), TrackerError::Parameter, "{c:?}");
        assert_eq!(s, snapshot, "{c:?} changed the session");
    }
    // A canceled analysis is pigment's typed refusal.
    run(&mut s, TrackerCommand::SetCurrentFrame { frame: 0 }, &f).unwrap();
    run(&mut s, TrackerCommand::SetPointsBarIn, &f).unwrap();
    let cancel = CancellationToken::default();
    cancel.cancel();
    let e = s.apply(TrackerCommand::Analyze { direction: TrackDirection::Forward }, &f, &TrackParams::DEFAULT, &TrackLimits::DEFAULT, &cancel, &mut |_, _| {});
    assert_eq!(e.unwrap_err(), TrackerError::Track(TrackError::Canceled));
    assert!(TrackerSession::new(0).is_err() && TrackerSession::new(MAX_SESSION_FRAMES + 1).is_err());
}
