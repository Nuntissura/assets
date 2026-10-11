use hsk_studio_accord::{CancellationToken, FrameRate, Ticks};
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

fn rate(ticks_per_frame: u64) -> FrameRate {
    timebase_from_import(ticks_per_frame).unwrap().0
}

fn r(start: u64, end: u64) -> TickRange {
    TickRange::new(Ticks::new(start), Ticks::new(end)).unwrap()
}

fn t(v: u64) -> Ticks {
    Ticks::new(v)
}

fn piece(origin: usize, range: TickRange, head: u64, tail: u64) -> LanePiece {
    LanePiece {
        origin,
        range,
        head_cut: t(head),
        tail_cut: t(tail),
    }
}

#[test]
fn lane_scheduling_and_cuts() {
    bounded_case("lane_scheduling_and_cuts", || {
        let token = CancellationToken::default();
        let free = Grid::Free;
        let lane = vec![r(0, 10), r(10, 20), r(25, 30)];
        // Active range at a tick (half-open) and edit-point navigation.
        let at = |v| range_at(&lane, t(v));
        assert_eq!(
            (at(5), at(10), at(20), at(22), at(25), at(30)),
            (Some(0), Some(1), None, None, Some(2), None)
        );
        let next = |v| next_edit_point(&lane, t(v)).map(Ticks::value);
        assert_eq!(
            (next(0), next(10), next(20), next(25), next(30)),
            (Some(10), Some(20), Some(25), Some(30), None)
        );
        let prev = |v| prev_edit_point(&lane, t(v)).map(Ticks::value);
        assert_eq!(
            (prev(30), prev(25), prev(20), prev(10), prev(0)),
            (Some(25), Some(20), Some(10), Some(0), None)
        );
        assert_eq!(gaps(&lane, &token).unwrap(), vec![r(20, 25)]);
        assert_eq!(
            close_gap(&lane, 0, &token).unwrap(),
            vec![r(0, 10), r(10, 20), r(20, 25)]
        );
        assert_eq!(
            close_gap(&lane, 1, &token).unwrap_err(),
            PulseError::OutsideRange
        );
        // Lift leaves a gap; pieces report how much head/tail was cut from their origin.
        assert_eq!(
            lift_span(&lane, r(5, 12), free, &token).unwrap(),
            vec![
                piece(0, r(0, 5), 0, 5),
                piece(1, r(12, 20), 2, 0),
                piece(2, r(25, 30), 0, 0)
            ]
        );
        // Extract closes the span (7 ticks): later content shifts earlier by exactly the span.
        assert_eq!(
            extract_span(&lane, r(5, 12), free, &token).unwrap(),
            vec![
                piece(0, r(0, 5), 0, 5),
                piece(1, r(5, 13), 2, 0),
                piece(2, r(18, 23), 0, 0)
            ]
        );
        // A fully covered range disappears; a straddled range splits into two pieces.
        assert_eq!(
            extract_span(&lane, r(8, 22), free, &token).unwrap(),
            vec![piece(0, r(0, 8), 0, 2), piece(2, r(11, 16), 0, 0)]
        );
        let single = [r(0, 100)];
        assert_eq!(
            lift_span(&single, r(40, 60), free, &token).unwrap(),
            vec![piece(0, r(0, 40), 0, 60), piece(0, r(60, 100), 60, 0)]
        );
        assert_eq!(
            extract_span(&single, r(40, 60), free, &token).unwrap(),
            vec![piece(0, r(0, 40), 0, 60), piece(0, r(40, 80), 60, 0)]
        );
        let r25 = rate(10_160_640_000);
        assert_eq!(
            lift_span(&lane, r(5, 12), Grid::Frames(r25), &token).unwrap_err(),
            PulseError::NotFrameAligned
        );
        assert_eq!(
            lift_span(&lane, r(5, 5), free, &token).unwrap_err(),
            PulseError::ZeroDuration
        );
        // Roll: the edit point moves, the extent does not.
        let pair = [r(0, 10), r(10, 20)];
        let delta = |v: i128| TickDelta::new(v).unwrap();
        assert_eq!(
            roll_edit(&pair, 0, delta(3), free, &token).unwrap(),
            vec![r(0, 13), r(13, 20)]
        );
        assert_eq!(
            roll_edit(&pair, 0, delta(-10), free, &token).unwrap_err(),
            PulseError::ZeroDuration
        );
        assert_eq!(
            roll_edit(&pair, 0, delta(10), free, &token).unwrap_err(),
            PulseError::ZeroDuration
        );
        assert_eq!(
            roll_edit(&pair, 0, delta(11), free, &token).unwrap_err(),
            PulseError::OutsideRange
        );
        assert_eq!(
            roll_edit(&[r(0, 10), r(25, 30)], 0, delta(1), free, &token).unwrap_err(),
            PulseError::NotAdjacent
        );
        // Slide: previous lengthens, next shortens, the slid range keeps its length.
        let three = [r(0, 10), r(10, 20), r(20, 30)];
        assert_eq!(
            slide(&three, 1, delta(4), free, &token).unwrap(),
            vec![r(0, 14), r(14, 24), r(24, 30)]
        );
        assert_eq!(
            slide(&three, 1, delta(-4), free, &token).unwrap(),
            vec![r(0, 6), r(6, 16), r(16, 30)]
        );
        assert_eq!(
            slide(&three, 1, delta(-10), free, &token).unwrap_err(),
            PulseError::ZeroDuration
        );
        assert_eq!(
            slide(&three, 0, delta(1), free, &token).unwrap_err(),
            PulseError::NotAdjacent
        );
        assert_eq!(
            slide(&three, 2, delta(1), free, &token).unwrap_err(),
            PulseError::OutsideRange
        );
    });
}

#[test]
fn display_parse_and_snap() {
    bounded_case("display_parse_and_snap", || {
        // Typed timecode entry.
        let df = parse_timecode("00:01:00;02").unwrap();
        assert_eq!(
            df,
            Timecode {
                hours: 0,
                minutes: 1,
                seconds: 0,
                frames: 2,
                drop_frame: true
            }
        );
        assert!(!parse_timecode("01:02:03:04").unwrap().drop_frame);
        for bad in [
            "",
            "1:2:3",
            "00;01:00:00",
            "aa:00:00:00",
            "00:00:00:300",
            "00:00:00:0:0",
            "00:00:00:-1",
        ] {
            assert_eq!(
                parse_timecode(bad).unwrap_err(),
                PulseError::InvalidTimecode,
                "{bad}"
            );
        }
        let r2997 = rate(8_475_667_200);
        assert_eq!(
            timecode_to_ticks(df, r2997, true).unwrap().value(),
            1800 * 8_475_667_200
        );
        // Frame snapping with explicit rounding.
        let r25 = rate(10_160_640_000);
        let tpf = 10_160_640_000u64;
        let half = t(3 * tpf + tpf / 2);
        assert_eq!(
            snap_to_frame(half, r25, Rounding::Floor).unwrap().value(),
            3 * tpf
        );
        assert_eq!(
            snap_to_frame(half, r25, Rounding::Ceil).unwrap().value(),
            4 * tpf
        );
        assert_eq!(
            snap_to_frame(half, r25, Rounding::NearestHalfUp)
                .unwrap()
                .value(),
            4 * tpf
        );
        assert_eq!(
            snap_to_frame(t(3 * tpf + 1), r25, Rounding::NearestHalfUp)
                .unwrap()
                .value(),
            3 * tpf
        );
        assert_eq!(
            snap_to_frame(half, r25, Rounding::Exact).unwrap_err(),
            PulseError::NotFrameAligned
        );
        assert_eq!(
            snap_to_frame(t(3 * tpf), r25, Rounding::Exact)
                .unwrap()
                .value(),
            3 * tpf
        );
        // Display enumeration (STU-VID-012a/b).
        for code in (100..=112).chain([200, 201]) {
            assert_eq!(TimeDisplay::from_code(code).unwrap().code(), code);
        }
        assert_eq!(
            TimeDisplay::from_code(113).unwrap_err(),
            PulseError::InvalidInput
        );
        assert_eq!(
            TimeDisplay::from_code(5).unwrap_err(),
            PulseError::InvalidInput
        );
        assert_eq!(
            time_display_from_import(113, r25).unwrap(),
            (
                TimeDisplay::Timecode25,
                Some(DisplayImportWarning::UnknownCode113)
            )
        );
        assert_eq!(
            time_display_from_import(113, rate(20_321_280_000)).unwrap(),
            (
                TimeDisplay::Frames,
                Some(DisplayImportWarning::UnknownCode113)
            )
        );
        // Display is independent of the sequence rate: a 25 fps sequence may show 24 fps timecode.
        assert_eq!(
            time_display_from_import(102, r25).unwrap(),
            (TimeDisplay::Timecode2997Drop, None)
        );
        let forty_seconds = frames_to_ticks(1000, r25).unwrap();
        let show = |d, hz| format_ticks(forty_seconds, r25, d, hz);
        assert_eq!(show(TimeDisplay::Timecode24, None).unwrap(), "00:00:40:00");
        assert_eq!(show(TimeDisplay::Timecode25, None).unwrap(), "00:00:40:00");
        assert_eq!(show(TimeDisplay::Frames, None).unwrap(), "1000");
        assert_eq!(show(TimeDisplay::Milliseconds, None).unwrap(), "40000");
        assert_eq!(
            show(TimeDisplay::AudioSamples, Some(48_000)).unwrap(),
            "1920000"
        );
        assert_eq!(
            show(TimeDisplay::AudioSamples, None).unwrap_err(),
            PulseError::InvalidInput
        );
        assert_eq!(
            show(TimeDisplay::FeetFrames35mm, None).unwrap_err(),
            PulseError::TimecodeUnsupported
        );
        let df_ticks = frames_to_ticks(1800, r2997).unwrap();
        assert_eq!(
            format_ticks(df_ticks, r25, TimeDisplay::Timecode2997Drop, None).unwrap(),
            "00:01:00;02"
        );
    });
}
