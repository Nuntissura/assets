//! Lane SMB-AE-ROTO-TIMEREMAP slice 1: time remapping (STU-MOT-045), time stretch / reverse and
//! the frame-blending sample rule (STU-MOT-007a) against hand-computed values. After Effects
//! numeric behaviour is UNVERIFIED (D5).
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND, Ticks};
use hsk_studio_motion::{
    EvalTick, FrameBlending, MotionError, RemapClamp, SourceTiming, TimeRemap, frame_sample,
};

/// One 24 fps frame in ticks.
const F: u64 = TICKS_PER_SECOND / 24;

fn tick(t: u64) -> EvalTick {
    EvalTick::new(i64::try_from(t).unwrap())
}

#[test]
fn enable_is_identity_and_clamps_hostile_values() {
    let cancel = CancellationToken::default();
    let source = SourceTiming::new(10 * F, F).unwrap();
    let in_point = 5 * F;
    let remap = TimeRemap::enable(Ticks::new(in_point), source).unwrap();
    // Contract 1: two keys, (in, 0) and (in + last frame, last frame); identity inside the layer.
    let keys = remap.property().keyframes();
    assert_eq!(keys.len(), 2);
    assert_eq!((keys[0].time.value(), keys[0].value[0]), (in_point, 0.0));
    assert_eq!(keys[1].time.value(), in_point + 9 * F);
    assert_eq!(keys[1].value[0], (9 * F) as f64 / TICKS_PER_SECOND as f64);
    for k in [0u64, 1, 4, 9] {
        let s = remap.sample(tick(in_point + k * F), &cancel).unwrap();
        assert_eq!((s.source_tick, s.clamp), (k * F, RemapClamp::None), "frame {k}");
    }
    // Out point unbounded (contract 4): after the last key the last frame is held.
    assert_eq!(remap.sample(tick(in_point + 40 * F), &cancel).unwrap().source_tick, 9 * F);

    // Contract 5: values outside the source clamp (never an error), reported.
    let hostile = TimeRemap::with_keys(source, &[(0, -1.0e300), (F, 1.0e300)]).unwrap();
    let lo = hostile.sample(tick(0), &cancel).unwrap();
    assert_eq!((lo.source_tick, lo.clamp, lo.source_seconds), (0, RemapClamp::First, 0.0));
    let hi = hostile.sample(tick(F), &cancel).unwrap();
    assert_eq!((hi.source_tick, hi.clamp), (9 * F, RemapClamp::Last));
    // A huge remap range between keys still clamps at every tick in between.
    let mid = hostile.sample(tick(F / 2 + 1), &cancel).unwrap();
    assert_eq!(mid.clamp, RemapClamp::Last);

    // NaN / infinite source times and ticks beyond i64 refuse typed; nothing is clamped silently there.
    assert_eq!(TimeRemap::with_keys(source, &[(0, f64::NAN)]).unwrap_err(), MotionError::NonFinite);
    assert_eq!(TimeRemap::with_keys(source, &[(0, f64::INFINITY)]).unwrap_err(), MotionError::NonFinite);
    assert_eq!(TimeRemap::with_keys(source, &[(u64::MAX, 0.0)]).unwrap_err(), MotionError::TickOverflow);
    assert!(matches!(TimeRemap::with_keys(source, &[]), Err(MotionError::InvalidParameter { .. })));
    assert_eq!(
        TimeRemap::from_stretch(Ticks::new(0), source, i64::MAX, 1).unwrap_err(),
        MotionError::TickOverflow
    );
    assert!(matches!(TimeRemap::from_stretch(Ticks::new(0), source, 0, 1), Err(MotionError::InvalidParameter { .. })));
    assert!(matches!(SourceTiming::new(F, 0), Err(MotionError::InvalidParameter { .. })));
    assert!(matches!(SourceTiming::new(F, 2 * F), Err(MotionError::InvalidParameter { .. })));
    assert_eq!(SourceTiming::new(u64::MAX, F).unwrap_err(), MotionError::TickOverflow);
    // Cancellation never yields a value.
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(remap.sample(tick(in_point), &canceled).unwrap_err(), MotionError::Canceled);
}

#[test]
fn stretch_reverse_and_frame_blending_hand_values() {
    let cancel = CancellationToken::default();
    let source = SourceTiming::new(10 * F, F).unwrap();
    // 200 % stretch (half speed): composition frame 3 shows source time 1.5 frames.
    let slow = TimeRemap::from_stretch(Ticks::new(0), source, 2, 1).unwrap();
    assert_eq!(slow.property().keyframes()[1].time.value(), 18 * F);
    let s = slow.sample(tick(3 * F), &cancel).unwrap();
    assert_eq!(s.source_tick, 3 * F / 2);
    let off = frame_sample(s.source_tick, source, FrameBlending::Off);
    assert_eq!((off.frame_a, off.frame_b, off.weight_b), (1, 1, 0.0));
    let mix = slow.frame_at(tick(3 * F), FrameBlending::FrameMix, &cancel).unwrap();
    assert_eq!((mix.frame_a, mix.frame_b, mix.weight_b), (1, 2, 0.5));
    let pm = slow.frame_at(tick(F), FrameBlending::PixelMotion, &cancel).unwrap();
    assert_eq!((pm.frame_a, pm.frame_b, pm.weight_b, pm.blending), (0, 1, 0.5, FrameBlending::PixelMotion));
    // On a whole source frame no second frame is blended; at the last frame the pair is held.
    assert_eq!(frame_sample(4 * F, source, FrameBlending::FrameMix).weight_b, 0.0);
    let last = frame_sample(9 * F + F / 3, source, FrameBlending::FrameMix);
    assert_eq!((last.frame_a, last.frame_b, last.weight_b), (9, 9, 0.0));

    // Negative stretch reverses: first composition frame shows the last source frame.
    let back = TimeRemap::from_stretch(Ticks::new(0), source, -1, 1).unwrap();
    assert_eq!(back.sample(tick(0), &cancel).unwrap().source_tick, 9 * F);
    assert_eq!(back.sample(tick(9 * F), &cancel).unwrap().source_tick, 0);
    assert_eq!(back.sample(tick(2 * F), &cancel).unwrap().source_tick, 7 * F);
    // Time-reverse keyframes of the identity remap is the -100 % stretch (contract 3: decreasing
    // segment plays backwards), and reversing twice restores it.
    let mut rev = TimeRemap::enable(Ticks::new(0), source).unwrap();
    rev.reverse().unwrap();
    assert_eq!(rev.property().keyframes(), back.property().keyframes());
    rev.reverse().unwrap();
    assert_eq!(rev.property().keyframes(), TimeRemap::enable(Ticks::new(0), source).unwrap().property().keyframes());
    // A flat segment freezes (contract 3).
    let freeze = TimeRemap::with_keys(source, &[(0, 0.125), (8 * F, 0.125)]).unwrap();
    for t in [0, 3 * F, 8 * F] {
        assert_eq!(freeze.sample(tick(t), &cancel).unwrap().source_tick, 3 * F);
    }
    assert_eq!(FrameBlending::parse("pixel_motion"), Some(FrameBlending::PixelMotion));
    assert_eq!(FrameBlending::FrameMix.wire(), "frame_mix");
    assert_eq!(FrameBlending::parse("Frame Mix"), None);
}
