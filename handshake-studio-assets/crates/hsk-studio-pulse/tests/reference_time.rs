//! Media Foundation REFERENCE_TIME (100 ns) <-> ticks: rule, receipt, round trip, range.
//! Expected values are hand/Python-Fraction computed (1 unit = 127008/5 = 25401.6 ticks), not
//! produced by the code under test.
use hsk_studio_accord::{Rounding, TICKS_PER_SECOND, Ticks};
use hsk_studio_pulse::*;

const FLOOR: Rounding = Rounding::Floor;
const CEIL: Rounding = Rounding::Ceil;
const NEAR: Rounding = Rounding::NearestHalfUp;
/// Largest |100 ns count| whose tick value fits i64 (all three modes agree): floor(i64::MAX * 5 / 127008).
const N_MAX: i64 = 363_102_010_773_131;

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

/// Independent round-half-up of n * 127008 / 5: floor((2 * 127008 * n + 5) / 10).
fn nearest_forward(n: i64) -> i128 {
    (i128::from(n) * 127_008 * 2 + 5).div_euclid(10)
}

/// Independent round-half-up of t * 5 / 127008: floor((10 * t + 127008) / 254016).
fn nearest_backward(t: i64) -> i128 {
    (i128::from(t) * 10 + 127_008).div_euclid(254_016)
}

#[test]
fn forward_hand_values_and_residuals() {
    bounded_case("forward_hand_values_and_residuals", || {
        // (100 ns, mode, ticks, residual numerator over 5); residual = exact - ticks.
        let table: &[(i64, Rounding, i64, i64)] = &[
            (1, FLOOR, 25_401, 3),
            (1, CEIL, 25_402, -2),
            (1, NEAR, 25_402, -2),
            (2, FLOOR, 50_803, 1),
            (2, CEIL, 50_804, -4),
            (2, NEAR, 50_803, 1),
            (3, FLOOR, 76_204, 4),
            (3, CEIL, 76_205, -1),
            (3, NEAR, 76_205, -1),
            (4, FLOOR, 101_606, 2),
            (4, CEIL, 101_607, -3),
            (4, NEAR, 101_606, 2),
            (-1, FLOOR, -25_402, 2),
            (-1, CEIL, -25_401, -3),
            (-1, NEAR, -25_402, 2),
            // Media Foundation's rounded 29.97 and 23.976 frame durations are NOT frame-exact.
            (333_667, FLOOR, 8_475_675_667, 1),
            (333_667, CEIL, 8_475_675_668, -4),
            (333_667, NEAR, 8_475_675_667, 1),
            (416_667, NEAR, 10_584_008_467, 1),
        ];
        for &(input, mode, ticks, residual) in table {
            let r = ticks_from_reference_time(input, mode).unwrap();
            assert_eq!(
                (r.input_100ns, r.ticks, r.rounding, r.exact),
                (input, ticks, mode, false),
                "{input} {mode:?}"
            );
            assert_eq!(
                r.residual,
                Residual {
                    numerator: residual,
                    denominator: 5
                },
                "{input} {mode:?}"
            );
        }
        // Whole multiples of 5 are exact in every mode with a zero residual.
        let exact: &[(i64, i64)] = &[
            (0, 0),
            (5, 127_008),
            (-5, -127_008),
            (10_000_000, 254_016_000_000),
            // 25 fps frame = 40 ms = 400_000 units = 10_160_640_000 ticks (STU-VID-013).
            (400_000, 10_160_640_000),
        ];
        for &(input, ticks) in exact {
            for mode in [FLOOR, CEIL, NEAR] {
                let r = ticks_from_reference_time(input, mode).unwrap();
                assert_eq!((r.ticks, r.exact), (ticks, true), "{input} {mode:?}");
                assert!(r.residual.is_zero());
            }
        }
        assert_eq!(
            timebase_from_import(10_160_640_000)
                .unwrap()
                .0
                .ticks_per_frame(),
            10_160_640_000
        );
    });
}

#[test]
fn backward_hand_values_ties_and_sequence_ticks() {
    bounded_case("backward_hand_values_ties_and_sequence_ticks", || {
        // (ticks, mode, 100 ns, residual numerator over 127008); residual = exact - result.
        let table: &[(i64, Rounding, i64, i64)] = &[
            (1, FLOOR, 0, 5),
            (1, CEIL, 1, -127_003),
            (1, NEAR, 0, 5),
            (63_503, NEAR, 2, 63_499),
            // 63504 ticks = 2.5 units: a tie, NearestHalfUp goes toward +infinity.
            (63_504, FLOOR, 2, 63_504),
            (63_504, CEIL, 3, -63_504),
            (63_504, NEAR, 3, -63_504),
            (63_505, NEAR, 3, -63_499),
            (-63_504, FLOOR, -3, 63_504),
            (-63_504, CEIL, -2, -63_504),
            (-63_504, NEAR, -2, -63_504),
            (-1, FLOOR, -1, 127_003),
            (-1, NEAR, 0, -5),
        ];
        for &(input, mode, units, residual) in table {
            let r = reference_time_from_ticks(input, mode).unwrap();
            assert_eq!(
                (r.input_ticks, r.reference_time_100ns, r.rounding, r.exact),
                (input, units, mode, false),
                "{input} {mode:?}"
            );
            assert_eq!(
                r.residual,
                Residual {
                    numerator: residual,
                    denominator: 127_008
                },
                "{input} {mode:?}"
            );
        }
        for (ticks, units) in [
            (0, 0),
            (127_008, 5),
            (10_160_640_000, 400_000),
            (254_016_000_000, 10_000_000),
        ] {
            let r = reference_time_from_ticks(ticks, NEAR).unwrap();
            assert_eq!((r.reference_time_100ns, r.exact), (units, true));
            assert!(r.residual.is_zero());
        }
        // Sequence time (Ticks): one second; i64::MAX ticks; beyond i64 is a typed overflow.
        let one_second = reference_time_from_sequence_ticks(Ticks::new(TICKS_PER_SECOND), NEAR);
        assert_eq!(one_second.unwrap().reference_time_100ns, 10_000_000);
        let at_i64_max = reference_time_from_sequence_ticks(Ticks::new(i64::MAX as u64), NEAR);
        assert_eq!(at_i64_max.unwrap().reference_time_100ns, N_MAX);
        for beyond in [i64::MAX as u64 + 1, u64::MAX] {
            let r = reference_time_from_sequence_ticks(Ticks::new(beyond), NEAR);
            assert_eq!(r.unwrap_err(), PulseError::Overflow);
        }
    });
}

#[test]
fn round_trip_is_identity_under_nearest() {
    bounded_case("round_trip_is_identity_under_nearest", || {
        let check = |n: i64| {
            let forward = ticks_from_reference_time(n, NEAR).unwrap();
            assert_eq!(i128::from(forward.ticks), nearest_forward(n), "forward {n}");
            let back = reference_time_from_ticks(forward.ticks, NEAR).unwrap();
            assert_eq!(
                i128::from(back.reference_time_100ns),
                nearest_backward(forward.ticks),
                "backward {n}"
            );
            assert_eq!(back.reference_time_100ns, n, "round trip {n}");
            // Forward error is at most 2/5 tick.
            assert!(forward.residual.numerator.abs() <= 2, "residual {n}");
        };
        // Bounded exhaustive: every count in +-300_000 (60 ms, covers many multiples of 5).
        for n in -300_000..=300_000 {
            check(n);
        }
        // About 200_000 samples with a prime-ish stride across the whole i64-tick range.
        let mut n = -N_MAX;
        while n <= N_MAX {
            check(n);
            n += 3_631_020_107;
        }
        // Both i64 edges, a thousand values deep.
        for d in 0..=1000 {
            check(N_MAX - d);
            check(-N_MAX + d);
        }
        // Why NearestHalfUp is the interchange rule: Floor both ways is not an identity.
        let down = ticks_from_reference_time(1, FLOOR).unwrap().ticks;
        assert_eq!(down, 25_401);
        assert_eq!(
            reference_time_from_ticks(down, FLOOR)
                .unwrap()
                .reference_time_100ns,
            0
        );
        assert_eq!(REFERENCE_TIME_ROUNDING, NEAR);
    });
}

#[test]
fn range_edges_are_typed_overflow_never_panic() {
    bounded_case("range_edges_are_typed_overflow_never_panic", || {
        for mode in [FLOOR, CEIL, NEAR] {
            for beyond in [i64::MAX, i64::MIN, N_MAX + 1, -N_MAX - 1] {
                assert_eq!(
                    ticks_from_reference_time(beyond, mode).unwrap_err(),
                    PulseError::Overflow,
                    "{beyond} {mode:?}"
                );
            }
            // Backward never overflows, even at i64::MIN / i64::MAX.
            assert!(reference_time_from_ticks(i64::MIN, mode).is_ok());
            assert!(reference_time_from_ticks(i64::MAX, mode).is_ok());
        }
        // Edge values: N_MAX = 363_102_010_773_131 -> 9_223_372_036_854_764_409.6 ticks.
        let hi = |mode| ticks_from_reference_time(N_MAX, mode).unwrap();
        assert_eq!(hi(FLOOR).ticks, 9_223_372_036_854_764_409);
        assert_eq!(hi(FLOOR).residual.numerator, 3);
        assert_eq!(hi(CEIL).ticks, 9_223_372_036_854_764_410);
        assert_eq!(hi(NEAR).ticks, 9_223_372_036_854_764_410);
        assert_eq!(hi(NEAR).residual.numerator, -2);
        let lo = |mode| ticks_from_reference_time(-N_MAX, mode).unwrap();
        assert_eq!(lo(FLOOR).ticks, -9_223_372_036_854_764_410);
        assert_eq!(lo(FLOOR).residual.numerator, 2);
        assert_eq!(lo(CEIL).ticks, -9_223_372_036_854_764_409);
        assert_eq!(lo(NEAR).ticks, -9_223_372_036_854_764_410);
        // i64::MAX ticks = 363_102_010_773_131.363 units; i64::MIN ticks = -363_102_010_773_131.6.
        let max = |mode| reference_time_from_ticks(i64::MAX, mode).unwrap();
        assert_eq!(max(FLOOR).reference_time_100ns, N_MAX);
        assert_eq!(max(FLOOR).residual.numerator, 56_987);
        assert_eq!(max(CEIL).reference_time_100ns, N_MAX + 1);
        assert_eq!(max(CEIL).residual.numerator, -70_021);
        assert_eq!(max(NEAR).reference_time_100ns, N_MAX);
        let min = |mode| reference_time_from_ticks(i64::MIN, mode).unwrap();
        assert_eq!(min(FLOOR).reference_time_100ns, -N_MAX - 1);
        assert_eq!(min(FLOOR).residual.numerator, 70_016);
        assert_eq!(min(CEIL).reference_time_100ns, -N_MAX);
        assert_eq!(min(CEIL).residual.numerator, -56_992);
        assert_eq!(min(NEAR).reference_time_100ns, -N_MAX);
        // Negative reference time is a valid tick value but not sequence time.
        let negative = ticks_from_reference_time(-5, NEAR).unwrap();
        assert_eq!(negative.ticks, -127_008);
        assert_eq!(
            negative.sequence_ticks().unwrap_err(),
            PulseError::Underflow
        );
        let positive = ticks_from_reference_time(5, NEAR).unwrap();
        assert_eq!(positive.sequence_ticks().unwrap(), Ticks::new(127_008));
    });
}

#[test]
fn receipt_invariants_and_require_exact() {
    bounded_case("receipt_invariants_and_require_exact", || {
        // The reduced fraction is derived, not typed twice: 127008 / 5 = 254016000000 / 10^7.
        assert_eq!(
            (
                TICKS_PER_REFERENCE_UNIT_NUM,
                TICKS_PER_REFERENCE_UNIT_DEN,
                REFERENCE_TIME_UNITS_PER_SECOND
            ),
            (127_008, 5, 10_000_000)
        );
        assert_eq!(
            u128::from(TICKS_PER_REFERENCE_UNIT_NUM) * 10_000_000,
            u128::from(TICKS_PER_SECOND) * u128::from(TICKS_PER_REFERENCE_UNIT_DEN)
        );
        for mode in [FLOOR, CEIL, NEAR] {
            for n in -50..=50_i64 {
                let r = ticks_from_reference_time(n, mode).unwrap();
                assert_eq!(r.exact, n % 5 == 0, "{n} {mode:?}");
                assert_eq!(r.residual.is_zero(), r.exact);
                // exact value = ticks + residual / 5  <=>  n * 127008 == 5 * ticks + residual.
                assert_eq!(
                    i128::from(n) * 127_008,
                    5 * i128::from(r.ticks) + i128::from(r.residual.numerator)
                );
                let range = match mode {
                    Rounding::Floor => 0..=4,
                    Rounding::Ceil => -4..=0,
                    Rounding::NearestHalfUp => -2..=2,
                };
                assert!(range.contains(&r.residual.numerator), "{n} {mode:?}");
            }
            for t in -130_000..=130_000_i64 {
                let r = reference_time_from_ticks(t, mode).unwrap();
                assert_eq!(r.exact, t % 127_008 == 0, "{t} {mode:?}");
                assert_eq!(r.residual.is_zero(), r.exact);
                // t * 5 == 127008 * units + residual.
                assert_eq!(
                    i128::from(t) * 5,
                    127_008 * i128::from(r.reference_time_100ns) + i128::from(r.residual.numerator)
                );
            }
        }
        assert_eq!(
            ticks_from_reference_time(1, NEAR)
                .unwrap()
                .require_exact()
                .unwrap_err(),
            PulseError::Inexact
        );
        assert!(
            ticks_from_reference_time(5, NEAR)
                .unwrap()
                .require_exact()
                .is_ok()
        );
        assert_eq!(
            reference_time_from_ticks(1, NEAR)
                .unwrap()
                .require_exact()
                .unwrap_err(),
            PulseError::Inexact
        );
        assert!(
            reference_time_from_ticks(127_008, NEAR)
                .unwrap()
                .require_exact()
                .is_ok()
        );
    });
}
