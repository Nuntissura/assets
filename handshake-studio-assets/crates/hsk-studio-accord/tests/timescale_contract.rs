use hsk_studio_accord::{
    Conversion, FrameRate, Rounding, TICKS_PER_SECOND, Ticks, Timescale, ValidationError,
};

fn ticks(value: u64, exact: bool) -> Conversion<Ticks> {
    Conversion {
        value: Ticks::new(value),
        exact,
    }
}

#[test]
fn exact_at_media_audio_and_ntsc_timescales() {
    let n = Rounding::Floor;
    // 254016000000 / 90000 = 2_822_400; / 48000 = 5_292_000.
    assert_eq!(
        Ticks::from_timescale(90_000, 90_000, n).unwrap(),
        ticks(TICKS_PER_SECOND, true)
    );
    assert_eq!(
        Ticks::from_timescale(1, 90_000, n).unwrap(),
        ticks(2_822_400, true)
    );
    assert_eq!(
        Ticks::from_timescale(1, 48_000, n).unwrap(),
        ticks(5_292_000, true)
    );
    assert_eq!(
        Timescale::new(48_000).unwrap().exact_ticks_per_unit(),
        Some(5_292_000)
    );
    // One 29.97 / 23.976 frame is 1 unit at 30000/1001 and 24000/1001 frames/s.
    let ntsc30 = Timescale::rational(30_000, 1001).unwrap();
    let ntsc24 = Timescale::rational(24_000, 1001).unwrap();
    assert_eq!(
        ntsc30.ticks_from_units(1, n).unwrap(),
        ticks(8_475_667_200, true)
    );
    assert_eq!(
        ntsc24.ticks_from_units(1, n).unwrap(),
        ticks(10_594_584_000, true)
    );
    assert_eq!(ntsc30.exact_ticks_per_unit(), Some(8_475_667_200));
    // Same fraction in unreduced form is the same timescale; 1001 units at 30000 Hz is one frame.
    assert_eq!(Timescale::rational(60_000, 2002).unwrap(), ntsc30);
    assert_eq!(
        Ticks::from_timescale(1001, 30_000, n).unwrap(),
        ticks(8_475_667_200, true)
    );
    // Inverse is exact on the way back.
    let back = Ticks::new(8_475_667_200).to_timescale(30_000, n).unwrap();
    assert_eq!(
        back,
        Conversion {
            value: 1001,
            exact: true
        }
    );
}

#[test]
fn hundred_ns_units_and_tie_rounding_are_explicit() {
    // 1 unit of 100 ns = 254016000000 / 10_000_000 = 25401.6 ticks.
    let unit = |r| Ticks::from_timescale(1, 10_000_000, r).unwrap();
    assert_eq!(unit(Rounding::Floor), ticks(25_401, false));
    assert_eq!(unit(Rounding::Ceil), ticks(25_402, false));
    assert_eq!(unit(Rounding::NearestHalfUp), ticks(25_402, false));
    // 5 units = 127008 ticks exactly: every mode agrees and reports exact.
    for r in [Rounding::Floor, Rounding::Ceil, Rounding::NearestHalfUp] {
        assert_eq!(
            Ticks::from_timescale(5, 10_000_000, r).unwrap(),
            ticks(127_008, true)
        );
    }
    // Ticks back to 100 ns units: 25401 ticks = 0.99998 units.
    let t = Timescale::new(10_000_000).unwrap();
    assert_eq!(
        t.units_from_ticks(Ticks::new(25_401), Rounding::NearestHalfUp)
            .unwrap(),
        Conversion {
            value: 1,
            exact: false
        }
    );
    assert_eq!(
        t.units_from_ticks(Ticks::new(25_401), Rounding::Floor)
            .unwrap(),
        Conversion {
            value: 0,
            exact: false
        }
    );
    // 8192 Hz: 254016000000 / 8192 = 31_007_812.5 ticks, a true tie. Ties go toward +infinity,
    // so the signed negative tie rounds toward zero (reel's media_to_ticks rule).
    let h = Timescale::new(8192).unwrap();
    let pos = |r| h.signed_ticks_from_units(1, r).unwrap();
    let neg = |r| h.signed_ticks_from_units(-1, r).unwrap();
    assert_eq!(
        pos(Rounding::NearestHalfUp),
        Conversion {
            value: 31_007_813,
            exact: false
        }
    );
    assert_eq!(
        neg(Rounding::NearestHalfUp),
        Conversion {
            value: -31_007_812,
            exact: false
        }
    );
    assert_eq!(neg(Rounding::Floor).value, -31_007_813);
    assert_eq!(neg(Rounding::Ceil).value, -31_007_812);
    // Signed 100 ns: -25401.6 -> nearest -25402, ceil -25401.
    let s = Timescale::new(10_000_000).unwrap();
    assert_eq!(
        s.signed_ticks_from_units(-1, Rounding::NearestHalfUp)
            .unwrap(),
        Conversion {
            value: -25_402,
            exact: false
        }
    );
    assert_eq!(
        s.signed_ticks_from_units(-1, Rounding::Ceil).unwrap().value,
        -25_401
    );
}

#[test]
fn zero_overflow_and_negative_results_are_typed() {
    assert_eq!(Timescale::new(0).unwrap_err(), ValidationError::OutOfRange);
    assert_eq!(
        Timescale::rational(30_000, 0).unwrap_err(),
        ValidationError::OutOfRange
    );
    assert_eq!(
        Ticks::from_timescale(1, 0, Rounding::Floor).unwrap_err(),
        ValidationError::OutOfRange
    );
    // Unsigned ticks overflow exactly one second past the u64 range at 1 Hz.
    let last = u64::MAX / TICKS_PER_SECOND;
    assert_eq!(
        Ticks::from_timescale(last, 1, Rounding::Floor)
            .unwrap()
            .value
            .value(),
        last * TICKS_PER_SECOND
    );
    assert_eq!(
        Ticks::from_timescale(last + 1, 1, Rounding::Floor).unwrap_err(),
        ValidationError::TickOverflow
    );
    // Signed ticks are i64: i64::MAX units at 1 Hz does not fit; a 128-bit intermediate that
    // cannot be held is the same typed error rather than a wrap.
    let one = Timescale::new(1).unwrap();
    assert_eq!(
        one.signed_ticks_from_units(i64::MAX, Rounding::Floor)
            .unwrap_err(),
        ValidationError::TickOverflow
    );
    let huge = Timescale::rational(u64::MAX, u64::MAX - 1).unwrap();
    assert_eq!(
        huge.ticks_from_units(u64::MAX, Rounding::Floor)
            .unwrap_err(),
        ValidationError::TickOverflow
    );
    // Whole units from u64::MAX ticks still converts (no spurious overflow at a Hz scale).
    assert!(
        Ticks::new(u64::MAX)
            .to_timescale(u32::MAX, Rounding::Floor)
            .is_ok()
    );
}

#[test]
fn rescale_between_timescales_and_frame_rate_fractions() {
    let media = Timescale::new(90_000).unwrap();
    let audio = Timescale::new(48_000).unwrap();
    // 900 units at 90 kHz = 10 ms = 480 samples at 48 kHz; 1001 units is 533.866... samples.
    assert_eq!(
        media.rescale(900, audio, Rounding::Floor).unwrap(),
        Conversion {
            value: 480,
            exact: true
        }
    );
    assert_eq!(
        media.rescale(1001, audio, Rounding::Floor).unwrap(),
        Conversion {
            value: 533,
            exact: false
        }
    );
    assert_eq!(
        media
            .rescale(-1001, audio, Rounding::NearestHalfUp)
            .unwrap()
            .value,
        -534
    );
    // fps <-> FrameRate: exact table rates only.
    let ntsc = FrameRate::from_fps(30_000, 1001).unwrap();
    assert_eq!(ntsc.ticks_per_frame(), 8_475_667_200);
    assert_eq!(ntsc.fps_fraction(), (30_000, 1001));
    assert_eq!(FrameRate::from_fps(60_000, 2002).unwrap(), ntsc);
    assert_eq!(
        FrameRate::from_fps(25, 1).unwrap().ticks_per_frame(),
        10_160_640_000
    );
    assert_eq!(FrameRate::from_fps(25, 1).unwrap().fps_fraction(), (25, 1));
    for (num, den) in [(0, 1), (30, 0), (7, 1), (10_000_000, 1), (1, 3)] {
        assert_eq!(
            FrameRate::from_fps(num, den).unwrap_err(),
            ValidationError::UnsupportedFrameRate,
            "{num}/{den}"
        );
    }
}
