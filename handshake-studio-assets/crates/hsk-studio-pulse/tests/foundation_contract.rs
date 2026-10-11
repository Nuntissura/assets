use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, TICKS_PER_SECOND, Ticks};
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort,
};
use hsk_studio_pulse::*;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 30s deadline: {e}"),
    }
}

fn rate(ticks_per_frame: u64) -> hsk_studio_accord::FrameRate {
    timebase_from_import(ticks_per_frame).unwrap().0
}

fn range(start: u64, end: u64) -> TickRange {
    TickRange::new(Ticks::new(start), Ticks::new(end)).unwrap()
}

#[test]
fn ripple_trim_fixture_25fps() {
    bounded_case("ripple_trim_fixture_25fps", || {
        let token = CancellationToken::default();
        let (r25, receipt) = timebase_from_import(10_160_640_000).unwrap();
        assert!(!receipt.changed);
        let f = |n| frames_to_ticks(n, r25).unwrap();
        let (a, b) = (
            TickRange::from_start_duration(Ticks::new(0), f(3)).unwrap(),
            TickRange::from_start_duration(f(3), f(3)).unwrap(),
        );
        let lane = vec![a, b];
        let grid = Grid::Frames(r25);
        // MT-35540 oracle: independent arithmetic 2 * 10_160_640_000 = 20_321_280_000.
        let out = ripple_trim_end(&lane, 0, f(2), grid, &token).unwrap();
        assert_eq!(out[0], range(0, 20_321_280_000));
        assert_eq!(
            out[1],
            range(20_321_280_000, 20_321_280_000 + 30_481_920_000)
        );
        assert_eq!(out[1].duration(), b.duration());
        assert_eq!(lane, vec![a, b], "input lane unchanged");
        // Plain trim opens a gap and moves nothing else.
        let plain = trim_end(&lane, 0, f(2), grid, &token).unwrap();
        assert_eq!((plain[0].end().value(), plain[1]), (20_321_280_000, b));
        // Extension ripples later content later.
        let longer = ripple_trim_end(&lane, 0, f(5), grid, &token).unwrap();
        assert_eq!(longer[1].start(), f(5));
        // Head ripple: start fixed, content after shifts earlier by the trimmed amount.
        let head = ripple_trim_start(&lane, 0, f(1), grid, &token).unwrap();
        assert_eq!((head[0], head[1].start()), (range(0, 20_321_280_000), f(2)));
        // ripple_shift on the gap-opened lane equals the ripple-trim result; on the untrimmed lane it collides.
        let shifted =
            ripple_shift(&plain, f(3), TickDelta::frames(-1, r25).unwrap(), &token).unwrap();
        assert_eq!(shifted, out);
        assert_eq!(
            ripple_shift(&lane, f(3), TickDelta::frames(-1, r25).unwrap(), &token).unwrap_err(),
            PulseError::Overlap
        );
    });
}

#[test]
fn legacy_rates_and_conversions() {
    bounded_case("legacy_rates_and_conversions", || {
        let (r, n) = timebase_from_import(10_594_594_594).unwrap();
        assert!(n.changed && n.canonical_ticks_per_frame == 10_594_584_000);
        assert_eq!(r.ticks_per_frame(), 10_594_584_000);
        let (r2, n2) = timebase_from_import(8_475_675_675).unwrap();
        assert!(n2.changed && r2.ticks_per_frame() == 8_475_667_200);
        assert!(!timebase_from_import(10_160_640_000).unwrap().1.changed);
        assert_eq!(
            timebase_from_import(7).unwrap_err(),
            PulseError::UnsupportedFrameRate
        );
        let r25 = rate(10_160_640_000);
        assert_eq!(
            frames_to_ticks(u64::MAX, r25).unwrap_err(),
            PulseError::Overflow
        );
        let t = frames_to_ticks(90_000, r25).unwrap();
        assert_eq!(t.value(), 90_000 * 10_160_640_000);
        assert_eq!(ticks_to_frames_exact(t, r25).unwrap(), 90_000);
        assert_eq!(
            ticks_to_frames_exact(Ticks::new(t.value() + 1), r25).unwrap_err(),
            PulseError::NotFrameAligned
        );
        assert_eq!(
            ticks_to_frames_floor(Ticks::new(t.value() + 1), r25),
            90_000
        );
        // fps fractions and interchange rate lookup.
        assert_eq!(fps_fraction(r2), (30_000, 1001));
        assert_eq!(fps_fraction(r), (24_000, 1001));
        assert_eq!(fps_fraction(r25), (25, 1));
        assert_eq!(frame_rate_from_fps(30_000, 1001).unwrap(), r2);
        assert_eq!(frame_rate_from_fps(25, 1).unwrap(), r25);
        assert_eq!(
            frame_rate_from_fps(30_000, 1002).unwrap_err(),
            PulseError::UnsupportedFrameRate
        );
        assert_eq!(
            frame_rate_from_fps(0, 1).unwrap_err(),
            PulseError::UnsupportedFrameRate
        );
        // Audio sample positions are exact integer ticks.
        assert_eq!(
            ticks_from_samples(48_000, 48_000).unwrap().value(),
            TICKS_PER_SECOND
        );
        assert_eq!(ticks_from_samples(1, 44_100).unwrap().value(), 5_760_000);
        assert_eq!(
            samples_from_ticks_exact(Ticks::new(5_760_000 * 3), 44_100).unwrap(),
            3
        );
        assert_eq!(
            samples_from_ticks_exact(Ticks::new(5), 44_100).unwrap_err(),
            PulseError::NotFrameAligned
        );
        // 7 Hz divides 254016000000 (504^2 * 10^6); 11 Hz does not.
        assert_eq!(ticks_from_samples(1, 7).unwrap().value(), 36_288_000_000);
        assert_eq!(
            ticks_from_samples(1, 11).unwrap_err(),
            PulseError::UnsupportedSampleRate
        );
        // Rational interchange with explicit rounding and loss flag.
        let one = ticks_from_rational_seconds(1001, 24_000, Rounding::Exact).unwrap();
        assert_eq!((one.ticks.value(), one.exact), (10_594_584_000, true));
        let denominator = 5_000_000_000_000;
        assert_eq!(
            ticks_from_rational_seconds(1, denominator, Rounding::Exact).unwrap_err(),
            PulseError::Inexact
        );
        let ticks_for = |m| ticks_from_rational_seconds(1, denominator, m).unwrap();
        assert_eq!(
            (
                ticks_for(Rounding::Floor).ticks.value(),
                ticks_for(Rounding::Ceil).ticks.value()
            ),
            (0, 1)
        );
        assert!(!ticks_for(Rounding::Floor).exact);
        let half = |m| {
            ticks_from_rational_seconds(1, 508_032_000_000, m)
                .unwrap()
                .ticks
                .value()
        };
        assert_eq!(
            (half(Rounding::Floor), half(Rounding::NearestHalfUp)),
            (0, 1)
        );
        assert_eq!(
            rational_seconds_from_ticks(Ticks::new(8_475_667_200)),
            (1001, 30_000)
        );
        assert_eq!(rational_seconds_from_ticks(Ticks::new(0)), (0, 1));
        // Speed is a reduced rational with exact span arithmetic.
        assert_eq!(Speed::new(2, 4).unwrap(), Speed::new(1, 2).unwrap());
        assert_eq!(Speed::new(0, 1).unwrap_err(), PulseError::ZeroSpeed);
        assert_eq!(Speed::new(1, 0).unwrap_err(), PulseError::InvalidSpeed);
        assert_eq!(
            Speed::new(1, 2)
                .unwrap()
                .span_for_duration(Ticks::new(10))
                .unwrap()
                .value(),
            5
        );
        let reverse = Speed::new(-3, 2).unwrap();
        assert!(reverse.is_reversed());
        assert_eq!(reverse.duration_for_span(Ticks::new(9)).unwrap().value(), 6);
        assert_eq!(
            Speed::new(2, 3)
                .unwrap()
                .duration_for_span(Ticks::new(1))
                .unwrap_err(),
            PulseError::Inexact
        );
    });
}

fn label(ticks_per_frame: u64, drop: bool, frame: u64) -> String {
    let r = rate(ticks_per_frame);
    ticks_to_timecode(frames_to_ticks(frame, r).unwrap(), r, drop)
        .unwrap()
        .to_string()
}

#[test]
fn timecode_labels() {
    bounded_case("timecode_labels", || {
        const R2398: u64 = 10_594_584_000;
        const R24: u64 = 10_584_000_000;
        const R25: u64 = 10_160_640_000;
        const R2997: u64 = 8_475_667_200;
        const R5994: u64 = 4_237_833_600;
        assert_eq!(label(R24, false, 86_400), "01:00:00:00");
        assert_eq!(label(R25, false, 90_000), "01:00:00:00");
        assert_eq!(label(R25, false, 25 * 60 + 3), "00:01:00:03");
        // 23.976 NDF: an hour of labels is 3603.6 s of real time.
        assert_eq!(label(R2398, false, 86_400), "01:00:00:00");
        assert_eq!(
            frames_to_ticks(86_400, rate(R2398)).unwrap().value(),
            915_372_057_600_000
        );
        // 29.97: non-drop drifts, drop-frame (labels 0,1 skipped, not every tenth minute) tracks.
        assert_eq!(label(R2997, false, 17_982), "00:09:59:12");
        assert_eq!(label(R2997, true, 1799), "00:00:59;29");
        assert_eq!(label(R2997, true, 1800), "00:01:00;02");
        assert_eq!(label(R2997, true, 3598), "00:02:00;02");
        assert_eq!(label(R2997, true, 17_981), "00:09:59;29");
        assert_eq!(label(R2997, true, 17_982), "00:10:00;00");
        assert_eq!(label(R2997, true, 107_892), "01:00:00;00");
        // 59.94 DF drops 4 numbers per minute (FFmpeg libavutil/timecode.c rule, 35964 frames / 10 min).
        assert_eq!(label(R5994, true, 3599), "00:00:59;59");
        assert_eq!(label(R5994, true, 3600), "00:01:00;04");
        assert_eq!(label(R5994, true, 35_964), "00:10:00;00");
        assert_eq!(label(R5994, true, 215_784), "01:00:00;00");
        assert_eq!(label(R5994, false, 3600), "00:01:00:00");
        // Unsupported combinations and skipped labels are refused.
        let drop_on = |tpf: u64| TimecodeFormat::new(rate(tpf), true).unwrap_err();
        assert_eq!(drop_on(R25), PulseError::DropFrameUnsupported);
        assert_eq!(drop_on(R2398), PulseError::DropFrameUnsupported);
        assert_eq!(drop_on(8_467_200_000), PulseError::DropFrameUnsupported);
        assert_eq!(
            TimecodeFormat::new(rate(20_321_280_000), false).unwrap_err(),
            PulseError::TimecodeUnsupported
        );
        let df = TimecodeFormat::new(rate(R2997), true).unwrap();
        let tc = |m, s, f| Timecode {
            hours: 0,
            minutes: m,
            seconds: s,
            frames: f,
            drop_frame: true,
        };
        assert_eq!(
            timecode_to_frames(tc(1, 0, 0), df).unwrap_err(),
            PulseError::InvalidTimecode
        );
        assert_eq!(timecode_to_frames(tc(10, 0, 0), df).unwrap(), 17_982);
        assert_eq!(timecode_to_frames(tc(1, 0, 2), df).unwrap(), 1800);
        assert_eq!(
            timecode_to_ticks(tc(1, 0, 2), rate(R2997), true)
                .unwrap()
                .value(),
            1800 * R2997
        );
        // Round trip across all label kinds.
        for tpf in [R24, R25, R2997, R5994] {
            for drop in [false, true] {
                let Ok(format) = TimecodeFormat::new(rate(tpf), drop) else {
                    continue;
                };
                for frame in (0..300_000u64).step_by(7) {
                    assert_eq!(
                        timecode_to_frames(frames_to_timecode(frame, format), format).unwrap(),
                        frame
                    );
                }
            }
        }
    });
}

#[test]
fn lane_refusals() {
    bounded_case("lane_refusals", || {
        let token = CancellationToken::default();
        let r25 = rate(10_160_640_000);
        let grid = Grid::Frames(r25);
        let lane = vec![range(0, 10), range(12, 20)];
        assert_eq!(
            trim_end(&lane, 0, Ticks::new(13), Grid::Free, &token).unwrap_err(),
            PulseError::Overlap
        );
        assert_eq!(
            trim_start(&lane, 1, Ticks::new(9), Grid::Free, &token).unwrap_err(),
            PulseError::Overlap
        );
        assert_eq!(
            ripple_shift(&lane, Ticks::new(12), TickDelta::new(-5).unwrap(), &token).unwrap_err(),
            PulseError::Overlap
        );
        assert_eq!(
            ripple_shift(
                &[range(2, 10)],
                Ticks::new(0),
                TickDelta::new(-3).unwrap(),
                &token
            )
            .unwrap_err(),
            PulseError::Underflow
        );
        assert_eq!(
            ripple_shift(
                &[range(u64::MAX - 5, u64::MAX)],
                Ticks::new(0),
                TickDelta::new(10).unwrap(),
                &token
            )
            .unwrap_err(),
            PulseError::Overflow
        );
        assert_eq!(
            trim_end(&lane, 0, Ticks::new(0), Grid::Free, &token).unwrap_err(),
            PulseError::ZeroDuration
        );
        assert_eq!(
            trim_start(&lane, 0, Ticks::new(10), Grid::Free, &token).unwrap_err(),
            PulseError::ZeroDuration
        );
        assert_eq!(
            trim_start(&lane, 0, Ticks::new(11), Grid::Free, &token).unwrap_err(),
            PulseError::OutsideRange
        );
        assert_eq!(
            trim_end(&lane, 5, Ticks::new(1), Grid::Free, &token).unwrap_err(),
            PulseError::OutsideRange
        );
        let frame = frames_to_ticks(2, r25).unwrap().value();
        let video = vec![
            TickRange::from_start_duration(Ticks::new(0), frames_to_ticks(3, r25).unwrap())
                .unwrap(),
        ];
        assert_eq!(
            trim_end(&video, 0, Ticks::new(frame + 1), grid, &token).unwrap_err(),
            PulseError::NotFrameAligned
        );
        assert!(trim_end(&video, 0, Ticks::new(frame + 1), Grid::Free, &token).is_ok());
        assert_eq!(
            validate_lane(&[range(5, 9), range(0, 3)], &token).unwrap_err(),
            PulseError::UnorderedRanges
        );
        assert_eq!(
            validate_lane(&[range(0, 9), range(5, 12)], &token).unwrap_err(),
            PulseError::Overlap
        );
        assert_eq!(
            TickRange::new(Ticks::new(5), Ticks::new(4)).unwrap_err(),
            PulseError::InvalidRange
        );
        assert_eq!(
            range(0, 10).split_at(Ticks::new(10)).unwrap_err(),
            PulseError::OutsideRange
        );
        assert_eq!(
            range(0, 10).split_at(Ticks::new(4)).unwrap(),
            (range(0, 4), range(4, 10))
        );
        assert_eq!(range(0, 10).intersect(range(5, 20)), Some(range(5, 10)));
        assert_eq!(range(0, 5).intersect(range(5, 20)), None);
        assert_eq!(TickDelta::new(i128::MAX).unwrap_err(), PulseError::Overflow);
        assert_eq!(
            offset(Ticks::new(5), TickDelta::new(-6).unwrap()).unwrap_err(),
            PulseError::Underflow
        );
        assert_eq!(
            checked_sub(Ticks::new(1), Ticks::new(2)).unwrap_err(),
            PulseError::Underflow
        );
    });
}

struct VecSink(Vec<Vec<u8>>);
impl SinkPort for VecSink {
    fn try_send(
        &mut self,
        _class: DeliveryClass,
        bytes: &[u8],
    ) -> std::result::Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

fn observe() -> Observe {
    Observe::new(
        42,
        7,
        DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap(),
        ActorContext::new(
            "account",
            "principal",
            "owner",
            "owner-principal",
            "space",
            "session",
        )
        .unwrap(),
        Budget::new(0, 0).unwrap(),
    )
}

#[test]
fn cancel_and_observe() {
    bounded_case("cancel_and_observe", || {
        let live = CancellationToken::default();
        let r25 = rate(10_160_640_000);
        let lane = vec![range(0, 3 * 10_160_640_000)];
        let two = frames_to_ticks(2, r25).unwrap();
        let canceled = CancellationToken::default();
        canceled.cancel();
        let result = ripple_trim_end(&lane, 0, two, Grid::Frames(r25), &canceled);
        assert_eq!(result.clone().unwrap_err(), PulseError::Canceled);
        assert_eq!(
            validate_lane(&lane, &canceled).unwrap_err(),
            PulseError::Canceled
        );
        // Exactly one terminal frame per outcome; wire byte 5 = outcome, byte 6 = failure code.
        let frame_of = |result: &Result<Vec<TickRange>>, token: &CancellationToken| {
            let (mut o, mut sink) = (observe(), VecSink(Vec::new()));
            let receipt = report(result, 42, 7, token, &mut o, &mut sink).unwrap();
            assert_eq!(sink.0.len(), 1);
            (receipt.outcome, sink.0[0][5], sink.0[0][6])
        };
        assert_eq!(frame_of(&result, &canceled), (Outcome::Canceled, 4, 0));
        let ok = ripple_trim_end(&lane, 0, two, Grid::Frames(r25), &live);
        assert!(ok.is_ok());
        assert_eq!(frame_of(&ok, &live), (Outcome::Success, 2, 0));
        let bad = trim_end(&lane, 0, Ticks::new(0), Grid::Free, &live);
        assert_eq!(
            frame_of(&bad, &live),
            (Outcome::Failure(FailureCode::Validation), 3, 1)
        );
        let overflow = Err(PulseError::Overflow);
        assert_eq!(
            frame_of(&overflow, &live),
            (Outcome::Failure(FailureCode::Unsupported), 3, 4)
        );
        // The descriptor is balanced JSON naming the owner (string-aware bracket check).
        let (mut depth, mut in_str, mut esc) = (0i32, false, false);
        for ch in DESCRIPTOR.chars() {
            match (in_str, esc, ch) {
                (true, true, _) => esc = false,
                (true, false, '\\') => esc = true,
                (true, false, '"') => in_str = false,
                (true, false, c) => assert!(!c.is_control()),
                (false, _, '"') => in_str = true,
                (false, _, '{' | '[') => depth += 1,
                (false, _, '}' | ']') => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0);
        }
        assert!(depth == 0 && !in_str && DESCRIPTOR.contains("\"owner\":\"STUDIO-MODULE-PULSE\""));
    });
}
