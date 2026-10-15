//! Lane SMB-PULSE-PLAYBACK-2: loop, in/out play and J/K/L shuttle (STU-VID-023 transport: play
//! in-to-out, loop, an in point and an out point; transport table `cmd.transport.shuttle.left|
//! right|stop|slow.*`). Stated bounds: loop wrap is exact in integer ticks (the master position
//! after `k` samples is `in + (offset + k * 5292000 * speed) mod span`); with `DropLate` forward
//! presentations satisfy `0 <= position - frame_start < one frame`, reverse ones
//! `0 < frame_end - position <= one frame`; at Nx shuttle a refresh per real frame drops N - 1
//! frames. Premiere's shuttle steps are UNVERIFIED (D5).
use hsk_studio_accord::Ticks;
use hsk_studio_folio::*;
use hsk_studio_pulse::*;

/// 25 fps.
const F: u64 = 10_160_640_000;
/// 48 kHz samples per frame; ticks per sample.
const SPF: u64 = 1920;
const TPS: u64 = 5_292_000;

fn sequence(frames: u64) -> StudioSequence {
    StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(frames * F),
        tracks: Vec::new(),
        markers: Vec::new(),
        work_area: None,
    }
}

fn present(d: FrameDecision) -> Option<u64> {
    match d.action {
        FrameAction::Present { frame, .. } => Some(frame),
        _ => None,
    }
}

#[test]
fn loop_wraps_exactly_and_shuttles_in_both_directions() {
    let seq = sequence(100);
    let mut s = PlaybackScheduler::new(&seq, DropPolicy::DropLate).unwrap();
    let mut clock = HeadlessClock::new(48_000);
    clock.advance(31); // a running stream: anchoring is relative

    // In 10, out 19 (inclusive): a 10-frame span. Hostile spans are refused before any change.
    assert_eq!(s.set_in_out(19, 10).unwrap_err(), PulseError::InvalidRange);
    assert_eq!(s.set_in_out(10, 100).unwrap_err(), PulseError::OutsideRange);
    assert_eq!(s.set_in_out(u64::MAX, u64::MAX).unwrap_err(), PulseError::Overflow);
    assert_eq!(s.play_in_to_out(&clock).unwrap_err(), PulseError::InvalidRange);
    let span = s.set_in_out(10, 19).unwrap();
    assert_eq!((span.start(), span.end()), (Ticks::new(10 * F), Ticks::new(20 * F)));

    // Loop in-to-out: one refresh per frame shows 10..19, 10..19, 10.. exactly; master wraps.
    s.set_looping(true, &clock).unwrap();
    s.play_in_to_out(&clock).unwrap();
    let mut shown = Vec::new();
    for k in 0..25u64 {
        let d = s.poll(&clock).unwrap();
        assert_eq!(d.master, Ticks::new(10 * F + (k * F) % (10 * F)), "refresh {k}");
        shown.push(present(d).unwrap());
        clock.advance(SPF);
    }
    let want: Vec<u64> = (0..25).map(|k| 10 + k % 10).collect();
    assert_eq!(shown, want);
    assert_eq!((s.stats().wraps, s.stats().dropped), (2, 0));
    // Sub-frame exactness across the wrap: 25 frames + 7 samples after the anchor.
    clock.advance(7);
    assert_eq!(s.master(&clock).unwrap(), Ticks::new(15 * F + 7 * TPS));
    // A refresh two frames later shows frame 17 and drops the two it skipped (15, 16).
    clock.advance(2 * SPF);
    let d = s.poll(&clock).unwrap();
    assert_eq!(present(d), Some(17));
    assert_eq!(d.dropped, 2);

    // J: reverse 1x from where it is; 17,16,..,10 then wraps to 19 (looping).
    assert_eq!(s.shuttle_left(&clock).unwrap(), PlaySpeed::REVERSE);
    let mut back = Vec::new();
    for _ in 0..10 {
        clock.advance(SPF);
        let d = s.poll(&clock).unwrap();
        let frame = present(d).unwrap();
        // Reverse bound: 0 < frame_end - position <= one frame.
        let end = (frame + 1) * F;
        assert!(end > d.master.value() && end - d.master.value() <= F, "{d:?}");
        back.push(frame);
    }
    assert_eq!(back, [16, 15, 14, 13, 12, 11, 10, 19, 18, 17]);

    // L from reverse: 1x forward; L again 2x, again 4x, again 8x (cap), again 8x.
    assert_eq!(s.shuttle_right(&clock).unwrap(), PlaySpeed::FORWARD);
    assert_eq!(s.shuttle_right(&clock).unwrap(), PlaySpeed::new(2, 1).unwrap());
    assert_eq!(s.shuttle_right(&clock).unwrap(), PlaySpeed::new(4, 1).unwrap());
    // 4x: one refresh per real frame advances 4 frames and drops 3.
    s.set_looping(false, &clock).unwrap();
    s.scrub(Ticks::new(10 * F), &clock).unwrap();
    let before = s.stats().dropped;
    let mut fast = Vec::new();
    for _ in 0..3 {
        let d = s.poll(&clock).unwrap();
        fast.push((present(d).unwrap(), d.dropped));
        clock.advance(SPF);
    }
    assert_eq!(fast, [(10, 0), (14, 3), (18, 3)]);
    assert_eq!(s.stats().dropped - before, 6);
    assert_eq!(s.shuttle_right(&clock).unwrap(), PlaySpeed::new(8, 1).unwrap());
    assert_eq!(s.shuttle_right(&clock).unwrap(), PlaySpeed::new(8, 1).unwrap());
    // K: pause on the frame on screen.
    s.shuttle_stop(&clock).unwrap();
    assert_eq!(s.transport(), Transport::Paused);
}

#[test]
fn reverse_and_slow_play_end_at_the_range_edges_within_bounds() {
    let seq = sequence(100);

    // J from a paused playhead at frame 5 (no loop): 5,4,3,2,1,0, then End at the start.
    let mut s = PlaybackScheduler::new(&seq, DropPolicy::DropLate).unwrap();
    let mut clock = HeadlessClock::new(48_000);
    s.scrub(Ticks::new(5 * F + 99), &clock).unwrap();
    s.shuttle_left(&clock).unwrap();
    let mut frames = Vec::new();
    loop {
        let d = s.poll(&clock).unwrap();
        match d.action {
            FrameAction::Present { frame, .. } => frames.push(frame),
            FrameAction::Hold => {}
            FrameAction::End => break,
        }
        clock.advance(SPF);
    }
    assert_eq!(frames, [5, 4, 3, 2, 1, 0]);
    assert_eq!((s.transport(), s.playhead()), (Transport::Ended, Ticks::new(0)));
    // J again from the start: reverse play restarts at the last frame.
    s.play(&clock).unwrap();
    assert_eq!(present(s.poll(&clock).unwrap()), Some(99));

    // Irregular presenter in reverse with DropLate: every frame shown or dropped, bound held.
    let mut r = PlaybackScheduler::new(&seq, DropPolicy::DropLate).unwrap();
    let mut clock = HeadlessClock::new(48_000);
    r.set_speed(PlaySpeed::REVERSE, &clock).unwrap();
    r.play(&clock).unwrap();
    let (mut state, mut counted) = (11u64, 0u64);
    loop {
        let d = r.poll(&clock).unwrap();
        match d.action {
            FrameAction::Present { frame, .. } => {
                let end = (frame + 1) * F;
                assert!(end > d.master.value() && end - d.master.value() <= F);
                counted += 1 + d.dropped;
            }
            FrameAction::Hold => {}
            FrameAction::End => {
                counted += d.dropped;
                break;
            }
        }
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        clock.advance((state >> 33) % 4000 + 1);
    }
    assert_eq!(counted, 100);
    assert_eq!((r.stats().presented + r.stats().dropped, r.stats().leads), (100, 0));
    assert!(r.stats().max_lag_ticks <= F);

    // PresentEveryFrame at 2x reverse never drops; the lag grows and is reported.
    let mut e = PlaybackScheduler::new(&seq, DropPolicy::PresentEveryFrame).unwrap();
    let mut clock = HeadlessClock::new(48_000);
    e.set_speed(PlaySpeed::new(-2, 1).unwrap(), &clock).unwrap();
    e.play(&clock).unwrap();
    let mut shown = Vec::new();
    for _ in 0..4 {
        shown.push(present(e.poll(&clock).unwrap()).unwrap());
        clock.advance(SPF);
    }
    assert_eq!((shown, e.stats().dropped), (vec![99, 98, 97, 96], 0));
    assert!(e.stats().max_lag_ticks > F);

    // Shift+L: half speed forward; two real frames per sequence frame, exact positions.
    let mut h = PlaybackScheduler::new(&seq, DropPolicy::DropLate).unwrap();
    let mut clock = HeadlessClock::new(48_000);
    assert_eq!(h.shuttle_slow(false, &clock).unwrap(), PlaySpeed::new(1, 2).unwrap());
    clock.advance(3 * SPF + 1);
    assert_eq!(h.master(&clock).unwrap(), Ticks::new((3 * SPF + 1) * TPS / 2));
    assert_eq!(present(h.poll(&clock).unwrap()), Some(1));

    // Hostile speeds and clocks: typed refusals, no panic.
    assert_eq!(PlaySpeed::new(0, 1).unwrap_err(), PulseError::ZeroSpeed);
    for (n, d) in [(1, 3), (2, 2), (17, 2), (9, 1), (i32::MIN, 1), (1, 16), (1, 0)] {
        assert_eq!(PlaySpeed::new(n, d).unwrap_err(), PulseError::InvalidSpeed, "{n}/{d}");
    }
    let mut x = PlaybackScheduler::new(&seq, DropPolicy::DropLate).unwrap();
    let mut clock = HeadlessClock::new(48_000);
    x.set_speed(PlaySpeed::new(8, 1).unwrap(), &clock).unwrap();
    x.set_looping(true, &clock).unwrap();
    x.play(&clock).unwrap();
    clock.advance(u64::MAX);
    assert_eq!(x.poll(&clock).unwrap_err(), PulseError::Overflow);
}
