//! Lane SMB-AE-MOTION-TOOLS slice 2: text animators (STU-MOT-090..092). Selector weights and
//! per-character results are hand-computed from the maths stated in `text_animator.rs` (Studio's
//! reading; After Effects parity UNVERIFIED, D5).
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_motion::*;

const S: i64 = 254_016_000_000;

/// "ab cd": a, b (word 0), space, c, d (word 1), one line.
fn ab_cd() -> Vec<TextChar> {
    let c = |is_space, word| TextChar { is_space, word, line: 0 };
    vec![c(false, 0), c(false, 0), c(true, 0), c(false, 1), c(false, 1)]
}

fn range(start: f64, end: f64, shape: RangeShape) -> RangeSelector {
    RangeSelector { start, end, shape, ..RangeSelector::default() }
}

fn x(amounts: &[[f64; 3]]) -> Vec<f64> {
    amounts.iter().map(|a| a[0]).collect()
}

fn near(a: &[f64], b: &[f64]) {
    assert_eq!(a.len(), b.len());
    for (p, q) in a.iter().zip(b) {
        assert!((p - q).abs() < 1e-12, "{a:?} != {b:?}");
    }
}

fn amounts(sel: RangeSelector) -> Vec<f64> {
    x(&selector_amounts(&ab_cd(), &[Selector::Range(sel)], 0.0).unwrap())
}

#[test]
fn range_selector_weights_match_hand_computed_values() {
    // n = 5, 0..50 % => [0, 2.5] units. Smoothness 100 % = each unit's own coverage of [i, i+1].
    near(&amounts(range(0.0, 50.0, RangeShape::Square)), &[1.0, 1.0, 0.5, 0.0, 0.0]);
    // Smoothness 0 % = hard step at the centre (2.5 is not < 2.5).
    near(&amounts(RangeSelector { smoothness: 0.0, ..range(0.0, 50.0, RangeShape::Square) }), &[1.0, 1.0, 0.0, 0.0, 0.0]);
    // Ramp up 0..100 %: t = (i + 0.5) / 5.
    near(&amounts(range(0.0, 100.0, RangeShape::RampUp)), &[0.1, 0.3, 0.5, 0.7, 0.9]);
    near(&amounts(range(0.0, 100.0, RangeShape::RampDown)), &[0.9, 0.7, 0.5, 0.3, 0.1]);
    // Triangle: 1 - |2t - 1|.
    near(&amounts(range(0.0, 100.0, RangeShape::Triangle)), &[0.2, 0.6, 1.0, 0.6, 0.2]);
    // Round: sqrt(1 - (2t - 1)^2) at t = 0.1 -> sqrt(0.36) = 0.6; t = 0.3 -> sqrt(0.84).
    near(&amounts(range(0.0, 100.0, RangeShape::Round)), &[0.6, 0.84f64.sqrt(), 1.0, 0.84f64.sqrt(), 0.6]);
    // Smooth: (1 - cos(2 pi t)) / 2 at t = 0.5 -> 1; t = 0.1 -> (1 - cos 36 deg) / 2.
    let smooth = amounts(range(0.0, 100.0, RangeShape::Smooth));
    assert!((smooth[2] - 1.0).abs() < 1e-12 && (smooth[0] - (1.0 - 36f64.to_radians().cos()) / 2.0).abs() < 1e-12, "{smooth:?}");
    // Index units 1..3 => [1, 3].
    near(&amounts(RangeSelector { units: RangeUnits::Index, ..range(1.0, 3.0, RangeShape::Square) }), &[0.0, 1.0, 1.0, 0.0, 0.0]);
    // Offset 40 % moves 0..20 % to [2, 3]; start > end is the same range.
    near(&amounts(RangeSelector { offset: 40.0, ..range(20.0, 0.0, RangeShape::Square) }), &[0.0, 0.0, 1.0, 0.0, 0.0]);
    // Words: units a b -> 0, c d -> 1, the space belongs to none; 0..50 % of 2 words => word 0.
    near(&amounts(RangeSelector { based_on: BasedOn::Words, ..range(0.0, 50.0, RangeShape::Square) }), &[1.0, 1.0, 0.0, 0.0, 0.0]);
    // Excluding spaces: n = 4, t = (i + 0.5) / 4; the space gets 0.
    let ramp = RangeSelector { based_on: BasedOn::CharactersExcludingSpaces, ..range(0.0, 100.0, RangeShape::RampUp) };
    near(&amounts(ramp), &[0.125, 0.375, 0.0, 0.625, 0.875]);
    // Ease low 100 %: Bezier [0, 0, 2/3, 1] at v = 0.5 -> 3 * 0.5 * 0.25 * 2/3 + 0.125 = 0.375; amount 50 % halves.
    let eased = amounts(RangeSelector { ease_low: 100.0, amount: 50.0, ..range(0.0, 100.0, RangeShape::RampUp) });
    assert!((eased[2] - 0.1875).abs() < 1e-12, "{eased:?}");
    // Randomize order permutes the unit positions: the multiset of weights is the ramp's.
    let mut shuffled = amounts(RangeSelector { randomize_order: true, random_seed: 7, ..range(0.0, 100.0, RangeShape::RampUp) });
    assert_eq!(shuffled, amounts(RangeSelector { randomize_order: true, random_seed: 7, ..range(0.0, 100.0, RangeShape::RampUp) }), "seeded");
    shuffled.sort_by(f64::total_cmp);
    near(&shuffled, &[0.1, 0.3, 0.5, 0.7, 0.9]);
    // Two selectors multiply: square 0..60 % [1,1,1,0,0] times ramp up.
    let both = selector_amounts(
        &ab_cd(),
        &[Selector::Range(range(0.0, 60.0, RangeShape::Square)), Selector::Range(range(0.0, 100.0, RangeShape::RampUp))],
        0.0,
    )
    .unwrap();
    near(&x(&both), &[0.1, 0.3, 0.5, 0.0, 0.0]);
}

#[test]
fn animator_properties_resolve_per_character_and_a_keyframed_offset_travels() {
    let chars = ab_cd();
    let first = Animator {
        properties: AnimatorProperties {
            position: Some([0.0, -20.0]),
            scale: Some([200.0, 50.0]),
            rotation: Some(90.0),
            opacity: Some(0.0),
            fill: Some([1.0, 0.0, 0.0, 1.0]),
            tracking: Some(100.0),
        },
        selectors: vec![Selector::Range(range(0.0, 20.0, RangeShape::Square))],
    };
    // No selector = every character fully; amount 50 % fill lerps half way from the running fill.
    let second = Animator {
        properties: AnimatorProperties { position: Some([5.0, 0.0]), opacity: Some(50.0), ..AnimatorProperties::default() },
        selectors: vec![],
    };
    let third = Animator {
        properties: AnimatorProperties { fill: Some([1.0, 1.0, 1.0, 1.0]), ..AnimatorProperties::default() },
        selectors: vec![Selector::Range(RangeSelector { amount: 50.0, ..range(20.0, 40.0, RangeShape::Square) })],
    };
    let t = evaluate_text_animators(&chars, &[first, second, third], [0.0, 0.0, 1.0, 1.0], 0.0).unwrap();
    assert_eq!(t[0].position, [5.0, -20.0]);
    assert_eq!(t[0].scale, [2.0, 0.5]);
    assert_eq!((t[0].rotation, t[0].opacity, t[0].tracking), (90.0, 0.0, 100.0));
    assert_eq!(t[0].fill, Some([1.0, 0.0, 0.0, 1.0]));
    // Character 1: only the second animator (all) and half of the third.
    assert_eq!((t[1].position, t[1].scale, t[1].rotation, t[1].opacity), ([5.0, 0.0], [1.0, 1.0], 0.0, 0.5));
    assert_eq!(t[1].fill, Some([0.5, 0.5, 1.0, 1.0]));
    assert_eq!(t[2].fill, None);

    // Animating the SELECTOR (STU-MOT-090.3): offset 0 % at 0 s -> 80 % at 1 s, range 0..20 %, hard
    // edges. 0 s selects character 0, 0.5 s character 2 (offset 40 %), 1 s character 4.
    let base = Animator {
        properties: AnimatorProperties { opacity: Some(0.0), ..AnimatorProperties::default() },
        selectors: vec![Selector::Range(RangeSelector { smoothness: 0.0, ..range(0.0, 20.0, RangeShape::Square) })],
    };
    let offset = Property::new(
        "offset",
        vec![0.0],
        vec![Keyframe::new(Ticks::new(0), vec![0.0]), Keyframe::new(Ticks::new(S as u64), vec![80.0])],
    )
    .unwrap();
    let track = AnimatorTrack { base, streams: vec![(AnimatorParam::RangeOffset(0), offset)] };
    let cancel = CancellationToken::default();
    for (tick, selected) in [(0, 0usize), (S / 2, 2), (S, 4)] {
        let a = track.at(EvalTick::new(tick), &cancel).unwrap();
        let w = x(&selector_amounts(&chars, &a.selectors, 0.0).unwrap());
        let mut want = vec![0.0; 5];
        want[selected] = 1.0;
        assert_eq!(w, want, "tick {tick}");
    }
    let wrong = AnimatorTrack { base: Animator::default(), streams: vec![(AnimatorParam::RangeStart(3), Property::new("s", vec![0.0], vec![]).unwrap())] };
    assert_eq!(wrong.at(EvalTick::new(0), &cancel).err(), Some(MotionError::SelectionOutOfRange));
}

#[test]
fn wiggly_selector_is_seeded_bounded_and_correlated() {
    let chars = ab_cd();
    let w = WigglySelector {
        based_on: BasedOn::Characters,
        max_amount: 80.0,
        min_amount: 20.0,
        wiggles_per_second: 2.0,
        correlation: 0.0,
        temporal_phase: 0.0,
        spatial_phase: 0.0,
        lock_dimensions: false,
        random_seed: 11,
    };
    let a = selector_amounts(&chars, &[Selector::Wiggly(w)], 0.37).unwrap();
    assert!(a.iter().flatten().all(|v| (0.2..=0.8).contains(v)), "{a:?}");
    assert_eq!(a, selector_amounts(&chars, &[Selector::Wiggly(w)], 0.37).unwrap(), "same seed, same time");
    let other = selector_amounts(&chars, &[Selector::Wiggly(WigglySelector { random_seed: 12, ..w })], 0.37).unwrap();
    assert_ne!(a, other, "another seed");
    // min == max: exactly that amount.
    let flat = selector_amounts(&chars, &[Selector::Wiggly(WigglySelector { min_amount: 50.0, max_amount: 50.0, ..w })], 3.1).unwrap();
    assert!(flat.iter().flatten().all(|v| *v == 0.5));
    // Correlation 100 %: every unit identical; lock dimensions: x == y == z.
    let together = selector_amounts(&chars, &[Selector::Wiggly(WigglySelector { correlation: 100.0, lock_dimensions: true, ..w })], 1.23).unwrap();
    assert!(together.iter().all(|v| *v == together[0] && v[0] == v[1] && v[1] == v[2]), "{together:?}");
    // Lattice points: value noise at an integer x is the lattice value; smoothstep makes the
    // midpoint the mean of its two lattice values.
    let (p, q) = (value_noise(5, 1, 3.0), value_noise(5, 1, 4.0));
    assert!((value_noise(5, 1, 3.5) - (p + q) / 2.0).abs() < 1e-15);
}

#[test]
fn hostile_text_inputs_are_refused_before_allocation() {
    let chars = ab_cd();
    let nan = RangeSelector { start: f64::NAN, ..RangeSelector::default() };
    assert_eq!(selector_amounts(&chars, &[Selector::Range(nan)], 0.0), Err(MotionError::InvalidParameter { parameter: "range_start" }));
    let inf = RangeSelector { offset: f64::INFINITY, ..RangeSelector::default() };
    assert_eq!(selector_amounts(&chars, &[Selector::Range(inf)], 0.0), Err(MotionError::InvalidParameter { parameter: "range_offset" }));
    let ease = RangeSelector { ease_high: 101.0, ..RangeSelector::default() };
    assert_eq!(selector_amounts(&chars, &[Selector::Range(ease)], 0.0), Err(MotionError::InvalidParameter { parameter: "range_ease_high" }));
    let w = WigglySelector {
        based_on: BasedOn::Characters, max_amount: 10.0, min_amount: 20.0, wiggles_per_second: 1.0, correlation: 0.0,
        temporal_phase: 0.0, spatial_phase: 0.0, lock_dimensions: false, random_seed: 0,
    };
    assert_eq!(selector_amounts(&chars, &[Selector::Wiggly(w)], 0.0), Err(MotionError::InvalidParameter { parameter: "wiggly_min_amount" }));
    assert_eq!(selector_amounts(&chars, &[], f64::NAN), Err(MotionError::NonFinite));
    let many = vec![Selector::Range(RangeSelector::default()); MAX_SELECTORS + 1];
    assert_eq!(selector_amounts(&chars, &many, 0.0), Err(MotionError::CountLimit { what: "text_selectors" }));
    let bomb = vec![TextChar { is_space: false, word: 0, line: 0 }; MAX_TEXT_UNITS + 1];
    assert_eq!(evaluate_text_animators(&bomb, &[], [0.0; 4], 0.0), Err(MotionError::CountLimit { what: "text_units" }));
    let animators = vec![Animator::default(); MAX_ANIMATORS + 1];
    assert_eq!(evaluate_text_animators(&chars, &animators, [0.0; 4], 0.0), Err(MotionError::CountLimit { what: "text_animators" }));
    let bad_fill = Animator { properties: AnimatorProperties { fill: Some([2.0, 0.0, 0.0, 1.0]), ..AnimatorProperties::default() }, selectors: vec![] };
    assert_eq!(evaluate_text_animators(&chars, &[bad_fill], [0.0; 4], 0.0), Err(MotionError::InvalidParameter { parameter: "animator_fill" }));
}
