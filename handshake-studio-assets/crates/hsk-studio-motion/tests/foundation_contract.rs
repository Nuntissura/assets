//! Motion foundation contract (MT-35543 steps 1-6). Expected values are hand-computed from the
//! documented math in `keyframe.rs` and from ECMAScript Date vectors, independent of the
//! implementation.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, Ticks};
use hsk_studio_motion::*;
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, Observe, Outcome as ObserveOutcome, SinkPort,
};

const S: i64 = 254_016_000_000;

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

fn key(time: i64, value: &[f64]) -> Keyframe {
    Keyframe::new(Ticks::new(time as u64), value.to_vec())
}

fn at(property: &Property, tick: i64) -> Vec<f64> {
    property
        .sample(EvalTick::new(tick), &CancellationToken::default())
        .unwrap()
}

fn near(actual: &[f64], expected: &[f64], tolerance: f64) {
    assert_eq!(actual.len(), expected.len());
    for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() <= tolerance, "actual {actual:?} expected {expected:?}");
    }
}

fn pair(a: Keyframe, b: Keyframe, dimension: usize) -> Result<Property, MotionError> {
    Property::new("value", vec![0.0; dimension], vec![a, b])
}

#[test]
fn keyframe_sampling_analytic() {
    bounded_case("keyframe_sampling_analytic", || {
        // Linear scalar 0 -> 10 over 1 s: held ends, exact midpoint, negative effective ticks.
        let linear = pair(key(0, &[0.0]), key(S, &[10.0]), 1).unwrap();
        assert_eq!(at(&linear, -S), vec![0.0]);
        assert_eq!(at(&linear, -1), vec![0.0]);
        assert_eq!(at(&linear, 0), vec![0.0]);
        assert_eq!(at(&linear, 127_008_000_000), vec![5.0]);
        assert_eq!(at(&linear, S), vec![10.0]);
        assert_eq!(at(&linear, 2 * S), vec![10.0]);

        // Three components, 2 s segment: quarter and midpoint.
        let three = pair(key(0, &[0.0, 10.0, -4.0]), key(2 * S, &[10.0, 20.0, 4.0]), 3).unwrap();
        assert_eq!(at(&three, S), vec![5.0, 15.0, 0.0]);
        assert_eq!(at(&three, S / 2), vec![2.5, 12.5, -2.0]);

        // Hold out = step; a non-interpolable property holds every segment (MOT-031b).
        let hold = pair(
            key(0, &[0.0]).with_interpolation(Interpolation::Linear, Interpolation::Hold),
            key(S, &[10.0]),
            1,
        )
        .unwrap();
        assert_eq!((at(&hold, S / 2), at(&hold, S - 1), at(&hold, S)), (vec![0.0], vec![0.0], vec![10.0]));
        let frozen = linear.clone().with_interpolable(false).unwrap();
        assert_eq!((at(&frozen, S / 2), at(&frozen, S)), (vec![0.0], vec![10.0]));

        // Asymmetric in/out on one key: linear in, hold out (step to the next key).
        let asym = Property::new(
            "value",
            vec![0.0],
            vec![
                key(0, &[0.0]),
                key(S, &[10.0]).with_interpolation(Interpolation::Linear, Interpolation::Hold),
                key(2 * S, &[20.0]),
            ],
        )
        .unwrap();
        assert_eq!(at(&asym, S / 2), vec![5.0]);
        assert_eq!(at(&asym, S + S / 2), vec![10.0]);
        assert_eq!(at(&asym, 2 * S), vec![20.0]);

        // Default ease (speed 0, influence 0.16666666666) is odd-symmetric about the midpoint.
        let eased = pair(Keyframe::eased(Ticks::new(0), vec![0.0]), Keyframe::eased(Ticks::new(S as u64), vec![10.0]), 1).unwrap();
        near(&at(&eased, S / 2), &[5.0], 1e-12);
        let (early, late) = (at(&eased, S / 5)[0], at(&eased, S - S / 5)[0]);
        assert!((early + late - 10.0).abs() <= 1e-9, "{early} {late}");

        // Speed 0, influence 1/3 both sides: x(s)=s, y(s)=10(3s^2-2s^3); u=0.25 -> 1.5625.
        let third = TemporalTangent::new(0.0, 1.0 / 3.0).unwrap();
        let smooth = |a: Interpolation| {
            pair(
                key(0, &[0.0]).with_interpolation(a, a).with_tangents(vec![third], vec![third]),
                key(S, &[10.0]).with_interpolation(a, a).with_tangents(vec![third], vec![third]),
                1,
            )
            .unwrap()
        };
        let bez = smooth(Interpolation::Bezier);
        near(&at(&bez, S / 4), &[1.5625], 1e-12);
        near(&at(&bez, 3 * S / 4), &[8.4375], 1e-12);
        // Continuous / auto bezier evaluate from the same stored tangents.
        near(&at(&smooth(Interpolation::ContinuousBezier), S / 4), &[1.5625], 1e-12);
        near(&at(&smooth(Interpolation::AutoBezier), S / 4), &[1.5625], 1e-12);

        // Speed = chord slope with influence 1/3 reproduces a straight line.
        let slope = TemporalTangent::new(10.0, 1.0 / 3.0).unwrap();
        let straight = pair(
            key(0, &[0.0]).with_interpolation(Interpolation::Bezier, Interpolation::Bezier).with_tangents(vec![slope], vec![slope]),
            key(S, &[10.0]).with_interpolation(Interpolation::Bezier, Interpolation::Bezier).with_tangents(vec![slope], vec![slope]),
            1,
        )
        .unwrap();
        near(&at(&straight, S / 4), &[2.5], 1e-12);
        near(&at(&straight, 3 * S / 4), &[7.5], 1e-12);

        // Asymmetric segment, 2 s: out (speed 4, infl .5), in (speed -4, infl .25):
        // X=[0,.5,.75,1] Y=[0,4,10,8]; s=.5 -> x=19/32, y=6.25; t = 19/32 * 2 s = 301_644_000_000.
        let out = TemporalTangent::new(4.0, 0.5).unwrap();
        let arrive = TemporalTangent::new(-4.0, 0.25).unwrap();
        let skew = pair(
            key(0, &[0.0]).with_interpolation(Interpolation::Linear, Interpolation::Bezier).with_tangents(vec![], vec![out]),
            key(2 * S, &[8.0]).with_interpolation(Interpolation::Bezier, Interpolation::Linear).with_tangents(vec![arrive], vec![]),
            1,
        )
        .unwrap();
        near(&at(&skew, 301_644_000_000), &[6.25], 1e-9);
        // Endpoints are exact, never solver output.
        assert_eq!((at(&skew, 0), at(&skew, 2 * S)), (vec![0.0], vec![8.0]));

        // Mixed linear departure + bezier arrival: chord handle on the linear side, finite and
        // inside the key value range for a zero-speed arrival.
        let mixed = pair(
            key(0, &[0.0]),
            key(S, &[10.0]).with_interpolation(Interpolation::Bezier, Interpolation::Linear).with_tangents(vec![third], vec![]),
            1,
        )
        .unwrap();
        let v = at(&mixed, S / 2)[0];
        assert!(v > 5.0 && v < 10.0, "{v}");

        // Hard bounds report, never clamp: overshooting curve between in-bound keys.
        let wild = TemporalTangent::new(50.0, 1.0 / 3.0).unwrap();
        let bounded = pair(
            key(0, &[0.0]).with_interpolation(Interpolation::Linear, Interpolation::Bezier).with_tangents(vec![], vec![wild]),
            key(S, &[10.0]).with_interpolation(Interpolation::Bezier, Interpolation::Linear).with_tangents(vec![third], vec![]),
            1,
        )
        .unwrap()
        .with_bounds(Some(0.0), Some(10.0))
        .unwrap();
        assert_eq!(bounded.sample(EvalTick::new(S / 2), &CancellationToken::default()), Err(MotionError::OutOfBounds));

        // Cancellation is reported before any value.
        let token = CancellationToken::default();
        token.cancel();
        assert_eq!(linear.sample(EvalTick::new(0), &token), Err(MotionError::Canceled));
    });
}

#[test]
fn tangent_defaults_and_rejects() {
    bounded_case("tangent_defaults_and_rejects", || {
        assert_eq!(DEFAULT_INFLUENCE.to_bits(), 0.16666666666_f64.to_bits());
        assert_eq!(TemporalTangent::EASE.influence.to_bits(), DEFAULT_INFLUENCE.to_bits());
        assert_eq!(TemporalTangent::EASE.speed, 0.0);

        assert_eq!(TemporalTangent::new(0.0, 1.5), Err(MotionError::InvalidInfluence));
        assert_eq!(TemporalTangent::new(0.0, -0.1), Err(MotionError::InvalidInfluence));
        assert_eq!(TemporalTangent::new(0.0, f64::NAN), Err(MotionError::InvalidInfluence));
        assert_eq!(TemporalTangent::new(f64::NAN, 0.5), Err(MotionError::NonFinite));
        assert_eq!(TemporalTangent::new(f64::INFINITY, 0.5), Err(MotionError::NonFinite));
        assert!(TemporalTangent::new(-3.0, 0.0).is_ok() && TemporalTangent::new(3.0, 1.0).is_ok());

        let new = |keys: Vec<Keyframe>| Property::new("value", vec![0.0], keys).map(|_| ());
        let duplicate = new(vec![key(5, &[0.0]), key(5, &[1.0])]).unwrap_err();
        let unsorted = new(vec![key(9, &[0.0]), key(5, &[1.0])]).unwrap_err();
        let mismatch = new(vec![key(0, &[0.0]), key(5, &[1.0, 2.0])]).unwrap_err();
        let missing = new(vec![
            key(0, &[0.0]).with_interpolation(Interpolation::Linear, Interpolation::Bezier),
            key(5, &[1.0]),
        ])
        .unwrap_err();
        let nonfinite = new(vec![key(0, &[f64::NAN])]).unwrap_err();
        let overflow = Property::new("value", vec![0.0], vec![Keyframe::new(Ticks::new(u64::MAX), vec![0.0])]).map(|_| ()).unwrap_err();
        assert_eq!(
            [duplicate.code(), unsorted.code(), mismatch.code(), missing.code(), nonfinite.code(), overflow.code()],
            ["duplicate_key_time", "unsorted_keys", "component_mismatch", "missing_tangent", "nonfinite", "tick_overflow"]
        );
        // Tangent count must be 1 (broadcast) or the component count.
        let two = TemporalTangent::EASE;
        let bad_len = Property::new(
            "value",
            vec![0.0; 3],
            vec![Keyframe::new(Ticks::new(0), vec![0.0; 3]).with_interpolation(Interpolation::Linear, Interpolation::Bezier).with_tangents(vec![], vec![two, two])],
        )
        .map(|_| ())
        .unwrap_err();
        assert_eq!(bad_len, MotionError::ComponentMismatch);
        assert_eq!(Property::new("value", vec![], vec![]).map(|_| ()).unwrap_err(), MotionError::DimensionLimit);
        assert_eq!(Property::new("a/b", vec![0.0], vec![]).map(|_| ()).unwrap_err(), MotionError::InvalidKey);
        assert_eq!(Property::new("v", vec![5.0], vec![]).unwrap().with_bounds(Some(6.0), Some(1.0)).map(|_| ()).unwrap_err(), MotionError::InvalidBounds);
        assert_eq!(Property::new("v", vec![5.0], vec![]).unwrap().with_bounds(Some(6.0), None).map(|_| ()).unwrap_err(), MotionError::OutOfBounds);
        // No keys: the retained static value (MOT-020b).
        let still = Property::new("v", vec![2.0, 3.0], vec![]).unwrap();
        assert_eq!((still.state(), at(&still, 12345)), (PropertyState::Static, vec![2.0, 3.0]));
    });
}

struct Resolver;
impl ReferenceResolver for Resolver {
    fn resolve(&self, reference: &str) -> Resolution {
        match reference {
            "opacity" => Resolution::One(PropertyPath::new("c/l/opacity").unwrap()),
            "name" => Resolution::Many(vec![PropertyPath::new("c/l/a").unwrap(), PropertyPath::new("c/m/a").unwrap()]),
            _ => Resolution::None,
        }
    }
}

#[test]
fn dependency_cycle_and_order() {
    bounded_case("dependency_cycle_and_order", || {
        let p = |text: &str| PropertyPath::new(text).unwrap();
        let (a, b, c, d) = (p("p/a"), p("p/b"), p("p/c"), p("p/d"));
        let mut graph = DepGraph::new();
        for path in [&a, &b, &c, &d] {
            graph.declare(path.clone()).unwrap();
        }
        // A reads B, then B reads A: refused, both named, graph unchanged.
        graph.commit_expression(&a, std::slice::from_ref(&b)).unwrap();
        assert_eq!(
            graph.commit_expression(&b, std::slice::from_ref(&a)),
            Err(MotionError::Cycle { participants: vec![b.clone(), a.clone()] })
        );
        assert!(graph.reads_of(&b).is_empty());
        // Self read and three-property cycle.
        assert_eq!(graph.commit_expression(&c, std::slice::from_ref(&c)), Err(MotionError::Cycle { participants: vec![c.clone()] }));
        graph.commit_expression(&b, std::slice::from_ref(&c)).unwrap();
        assert_eq!(
            graph.commit_expression(&c, std::slice::from_ref(&a)),
            Err(MotionError::Cycle { participants: vec![c.clone(), a.clone(), b.clone()] })
        );
        // D reads B too. Dependencies first, ties by smallest path: C, B, A, D.
        graph.commit_expression(&d, std::slice::from_ref(&b)).unwrap();
        assert_eq!(graph.eval_order(), vec![c.clone(), b.clone(), a.clone(), d.clone()]);
        assert_eq!(graph.eval_order(), graph.eval_order());
        // Re-committing replaces the read set; empty set removes the edge.
        graph.commit_expression(&a, std::slice::from_ref(&c)).unwrap();
        assert_eq!(graph.reads_of(&a), vec![&c]);
        graph.commit_expression(&a, &[]).unwrap();
        assert!(graph.reads_of(&a).is_empty());
        // Undeclared reference is typed.
        assert_eq!(
            graph.commit_expression(&a, &[p("p/zz")]),
            Err(MotionError::ReferenceMissing { reference: "p/zz".to_owned() })
        );
        // Resolution: one / none / many.
        assert_eq!(resolve_references(&["opacity"], &Resolver).unwrap(), vec![p("c/l/opacity")]);
        assert_eq!(resolve_references(&["nope"], &Resolver), Err(MotionError::ReferenceMissing { reference: "nope".to_owned() }));
        assert_eq!(
            resolve_references(&["name"], &Resolver),
            Err(MotionError::AmbiguousReference { reference: "name".to_owned(), candidates: 2 })
        );
        // Long chains do not recurse: 20k-deep chain commits without stack growth.
        let mut chain = DepGraph::new();
        let nodes: Vec<PropertyPath> = (0..20_000).map(|i| p(&format!("n/{i:05}"))).collect();
        for node in &nodes {
            chain.declare(node.clone()).unwrap();
        }
        for pair in nodes.windows(2) {
            chain.commit_expression(&pair[0], &[pair[1].clone()]).unwrap();
        }
        let last = nodes.len() - 1;
        assert!(matches!(chain.commit_expression(&nodes[last], &[nodes[0].clone()]), Err(MotionError::Cycle { participants }) if participants.len() == 20_000));
        assert_eq!(chain.eval_order().first(), Some(&nodes[last]));
        // Path validation.
        for bad in ["", "a//b", "/a", "a/", "../x", "a/..", "a/./b", "a\u{0}b", &"x".repeat(257)] {
            assert_eq!(PropertyPath::new(bad), Err(MotionError::InvalidPath), "{bad:?}");
        }
    });
}

#[test]
fn date_clock_vectors() {
    bounded_case("date_clock_vectors", || {
        let ms = |profile: &DateProfile, tick: i64| profile.utc_ms(EvalTick::new(tick));
        let zero = DateProfile::new(0, 0).unwrap();
        assert_eq!(ms(&zero, 0), Ok(0));
        // 1 ms = 254_016_000 ticks; truncation is toward zero and zero is canonical.
        assert_eq!(ms(&zero, -254_016_000), Ok(-1));
        assert_eq!(ms(&zero, -127_008_000), Ok(0));
        assert_eq!(ms(&zero, -254_016_001), Ok(-1));
        assert_eq!(ms(&zero, 127_007_999), Ok(0));
        assert_eq!(ms(&zero, 254_016_000), Ok(1));
        assert_eq!(ms(&zero, -508_032_000), Ok(-2));
        let shifted = DateProfile::new(1000, S as u64).unwrap();
        assert_eq!((ms(&shifted, S), ms(&shifted, S + 254_016_000)), (Ok(1000), Ok(1001)));
        // Range edge: exact at the limit, any excess rejected, no panic at extreme ticks.
        let max = DateProfile::new(MAX_ABS_UTC_MS, 0).unwrap();
        let min = DateProfile::new(-MAX_ABS_UTC_MS, 0).unwrap();
        assert_eq!((ms(&max, 0), ms(&min, 0)), (Ok(MAX_ABS_UTC_MS), Ok(-MAX_ABS_UTC_MS)));
        assert_eq!(ms(&max, 254_016_000), Err(MotionError::DateOutOfRange));
        assert_eq!(ms(&max, 1), Err(MotionError::DateOutOfRange));
        assert_eq!(ms(&min, -1), Err(MotionError::DateOutOfRange));
        // i64 ticks span only ~420 days: independent integer division, truncation toward zero.
        assert_eq!(ms(&zero, i64::MAX), Ok(i64::MAX / 254_016_000));
        assert_eq!(ms(&zero, i64::MIN), Ok(i64::MIN / 254_016_000));
        assert_eq!(ms(&max, i64::MAX), Err(MotionError::DateOutOfRange));
        assert_eq!(ms(&min, i64::MIN), Err(MotionError::DateOutOfRange));
        assert_eq!(DateProfile::new(MAX_ABS_UTC_MS + 1, 0), Err(MotionError::InvalidDateProfile));
        assert_eq!(DateProfile::new(i64::MIN, 0), Err(MotionError::InvalidDateProfile));

        // ISO text (ECMAScript toISOString vectors).
        let iso = |value: i64| to_iso_string(value).unwrap();
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(MAX_ABS_UTC_MS), "+275760-09-13T00:00:00.000Z");
        assert_eq!(iso(-MAX_ABS_UTC_MS), "-271821-04-20T00:00:00.000Z");
        assert_eq!(iso(-62_167_219_200_000), "0000-01-01T00:00:00.000Z");
        assert_eq!(iso(-62_198_755_200_000), "-000001-01-01T00:00:00.000Z");
        assert_eq!(iso(253_402_300_799_999), "9999-12-31T23:59:59.999Z");
        assert_eq!(iso(253_402_300_800_000), "+010000-01-01T00:00:00.000Z");
        assert_eq!(iso(-1), "1969-12-31T23:59:59.999Z");
        assert_eq!(iso(1_709_210_096_789), "2024-02-29T12:34:56.789Z");
        assert_eq!(to_iso_string(MAX_ABS_UTC_MS + 1), None);
        assert_eq!(civil_from_ms(0).unwrap().weekday, 4);
        assert_eq!(civil_from_ms(-1).unwrap().weekday, 3);
        for value in [0, -1, 1, MAX_ABS_UTC_MS, -MAX_ABS_UTC_MS, -62_167_219_200_000, 1_709_210_096_789, 253_402_300_800_000] {
            assert_eq!(parse_iso_exact(&iso(value)), Some(value), "{value}");
        }
        assert_eq!(parse_iso_exact("2024-01-01T00:00:00Z"), Some(1_704_067_200_000));
        for bad in [
            "2024-01-01T00:00:00+01:00",
            "2024-01-01T00:00:00.000+00:00",
            "2024-01-01",
            "2024-01-01T00:00Z",
            "2024-01-01T24:00:00.000Z",
            "2024-01-01T00:00:60Z",
            "2024-01-01T00:00:00.12Z",
            "2024-01-01T00:00:00.1234Z",
            " 2024-01-01T00:00:00Z",
            "2024-01-01T00:00:00Z ",
            "2024-01-01 00:00:00Z",
            "2024-02-30T00:00:00Z",
            "2023-02-29T00:00:00Z",
            "2024-13-01T00:00:00Z",
            "-000000-01-01T00:00:00.000Z",
            "+275760-09-13T00:00:00.001Z",
            "+275761-01-01T00:00:00.000Z",
            "24-01-01T00:00:00Z",
            "",
        ] {
            assert_eq!(parse_iso_exact(bad), None, "{bad:?}");
        }
        assert_eq!(parse_iso_exact("2024-02-29T00:00:00Z"), Some(1_709_164_800_000));
        assert_eq!(parse_iso_exact("+275760-09-13T00:00:00.000Z"), Some(MAX_ABS_UTC_MS));
    });
}

enum Mode {
    Value(Vec<f64>),
    Base,
    Fail(ExprError),
    Spend(u64),
}

struct Double(Mode);

impl ExpressionProvider for Double {
    fn profile_id(&self) -> &'static str {
        "test-double"
    }

    fn evaluate(
        &self,
        request: &ExprRequest<'_>,
        _host: &dyn HostRead,
        fuel: &mut Fuel,
        _cancel: &CancellationToken,
    ) -> Result<Vec<f64>, ExprError> {
        match &self.0 {
            Mode::Value(value) => Ok(value.clone()),
            Mode::Base => Ok(request.base.expect("base").iter().map(|v| v + 1.0).collect()),
            Mode::Fail(error) => Err(*error),
            Mode::Spend(units) => {
                fuel.charge(*units)?;
                Ok(vec![1.0])
            }
        }
    }
}

struct Sink(Vec<(DeliveryClass, usize)>);
impl SinkPort for Sink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push((class, bytes.len()));
        Ok(())
    }
}

#[test]
fn expression_policy_fallback() {
    bounded_case("expression_policy_fallback", || {
        let cancel = CancellationToken::default();
        let run = |property: &mut Property, mode: Mode, tick: i64, fuel_limit: u64| {
            let ctx = EvalContext { expected_revision: property.revision(), tick: EvalTick::new(tick), seed: 99 };
            property.evaluate_with(&ctx, &Double(mode), &NoHost, &mut Fuel::new(fuel_limit), &cancel)
        };
        let ramp = || pair(key(0, &[0.0]), key(S, &[10.0]), 1).unwrap();

        // Expression-driven (no keys): provider value, receipt discloses profile/seed/fuel.
        let mut driven = Property::new("v", vec![3.0], vec![]).unwrap().with_expression("x").unwrap();
        assert_eq!(driven.configured_state(), PropertyState::ExpressionDriven);
        let done = run(&mut driven, Mode::Value(vec![7.0]), 0, 10).unwrap();
        assert_eq!(done.value, vec![7.0]);
        assert_eq!((done.receipt.profile, done.receipt.seed, done.receipt.fuel_used, done.receipt.revision), ("test-double", 99, 1, driven.revision()));
        assert_eq!((done.receipt.state, done.receipt.disabled_by_error), (PropertyState::ExpressionDriven, None));

        // Expression over keyframes receives the keyframed value as `base`.
        let mut over = ramp().with_expression("x").unwrap();
        assert_eq!(over.configured_state(), PropertyState::ExpressionOverKeyframes);
        assert_eq!(run(&mut over, Mode::Base, S / 2, 10).unwrap().value, vec![6.0]);

        // Expression error: disabled, falls back to keyframed value, receipt says so, stays off.
        let failure = ExprError::at(ExprErrorCode::Parse, 2, 5);
        let failed = run(&mut over, Mode::Fail(failure), S / 2, 10).unwrap();
        assert_eq!(failed.value, vec![5.0]);
        assert_eq!(failed.receipt.disabled_by_error, Some(ExprErrorCode::Parse));
        assert_eq!(failed.receipt.error_span, Some(SourceSpan { line: 2, column: 5 }));
        assert_eq!((failed.receipt.state, over.state()), (PropertyState::Keyframed, PropertyState::Keyframed));
        let again = run(&mut over, Mode::Value(vec![42.0]), S / 2, 10).unwrap();
        assert_eq!((again.value, again.receipt.fuel_used, again.receipt.disabled_by_error), (vec![5.0], 0, Some(ExprErrorCode::Parse)));
        // Fixing the expression advances the revision and re-enables it; a stale caller is rejected.
        let stale = EvalContext { expected_revision: over.revision(), tick: EvalTick::new(0), seed: 0 };
        over.set_expression(Some("fixed")).unwrap();
        assert_eq!(
            over.evaluate_with(&stale, &Double(Mode::Base), &NoHost, &mut Fuel::new(5), &cancel),
            Err(MotionError::StaleRevision { expected: stale.expected_revision, actual: over.revision() })
        );
        assert_eq!(run(&mut over, Mode::Value(vec![42.0]), 0, 10).unwrap().value, vec![42.0]);

        // Budget (shared fuel), wrong output shape and non-finite output all disable.
        let mut starved = ramp().with_expression("x").unwrap();
        assert_eq!(run(&mut starved, Mode::Value(vec![1.0]), 0, 0).unwrap().receipt.disabled_by_error, Some(ExprErrorCode::Budget));
        let mut spender = ramp().with_expression("x").unwrap();
        assert_eq!(run(&mut spender, Mode::Spend(10), 0, 5).unwrap().receipt.disabled_by_error, Some(ExprErrorCode::Budget));
        let mut shape = ramp().with_expression("x").unwrap();
        assert_eq!(run(&mut shape, Mode::Value(vec![1.0, 2.0]), 0, 5).unwrap().receipt.disabled_by_error, Some(ExprErrorCode::OutputType));
        let mut nan = ramp().with_expression("x").unwrap();
        assert_eq!(run(&mut nan, Mode::Value(vec![f64::NAN]), 0, 5).unwrap().receipt.disabled_by_error, Some(ExprErrorCode::OutputType));

        // Cancellation (provider or token) is an error, never a partial value, never a disable.
        let mut canceled = ramp().with_expression("x").unwrap();
        assert_eq!(run(&mut canceled, Mode::Fail(ExprError::canceled()), 0, 5).map(|_| ()), Err(MotionError::Canceled));
        assert_eq!(canceled.disabled_by_error(), None);
        let token = CancellationToken::default();
        token.cancel();
        let ctx = EvalContext { expected_revision: canceled.revision(), tick: EvalTick::new(0), seed: 0 };
        assert_eq!(canceled.evaluate_with(&ctx, &Double(Mode::Base), &NoHost, &mut Fuel::new(5), &token).map(|_| ()), Err(MotionError::Canceled));

        // Expression output outside hard bounds is reported, not clamped, not disabled.
        let mut tight = Property::new("v", vec![0.5], vec![]).unwrap().with_bounds(Some(0.0), Some(1.0)).unwrap().with_expression("x").unwrap();
        assert_eq!(run(&mut tight, Mode::Value(vec![5.0]), 0, 5).map(|_| ()), Err(MotionError::OutOfBounds));
        assert_eq!(tight.disabled_by_error(), None);

        // Pure closure: no provider -> `unsupported` disable, static fallback. Disabling the
        // expression returns the retained static value (MOT-020b).
        let mut pure = Property::new("v", vec![3.0], vec![]).unwrap().with_expression("x").unwrap();
        let ctx = EvalContext { expected_revision: pure.revision(), tick: EvalTick::new(0), seed: 0 };
        let evaluated = pure.evaluate_pure(&ctx, &cancel).unwrap();
        assert_eq!((evaluated.value, evaluated.receipt.disabled_by_error, evaluated.receipt.state), (vec![3.0], Some(ExprErrorCode::Unsupported), PropertyState::Static));
        let mut off = ramp().with_expression("x").unwrap();
        off.set_expression_enabled(false);
        assert_eq!(off.state(), PropertyState::Keyframed);
        let mut bare = Property::new("v", vec![3.0], vec![]).unwrap().with_expression("x").unwrap();
        bare.set_expression_enabled(false);
        assert_eq!((bare.state(), bare.static_value().to_vec()), (PropertyState::Static, vec![3.0]));
        assert_eq!(bare.set_expression(Some(&"x".repeat(MAX_EXPRESSION_BYTES + 1))), Err(MotionError::ExpressionTooLong));

        // Observability: closed outcome only, through a caller-owned sink.
        let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
        let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
        let emitter = |budget| Observe::new(1, 7, resource.clone(), actor.clone(), budget);
        let mut sink = Sink(Vec::new());
        let ok: Result<(), MotionError> = Ok(());
        let receipt = report(&ok, 1, 7, &cancel, &mut emitter(Budget::new(0, 0).unwrap()), &mut sink).unwrap();
        assert_eq!((receipt.sequence, receipt.outcome), (1, ObserveOutcome::Success));
        let rejected: Result<(), MotionError> = Err(MotionError::DuplicateKeyTime);
        let receipt = report(&rejected, 1, 7, &cancel, &mut emitter(Budget::new(0, 0).unwrap()), &mut sink).unwrap();
        assert!(matches!(receipt.outcome, ObserveOutcome::Failure(_)));
        let canceled: Result<(), MotionError> = Err(MotionError::Canceled);
        let receipt = report(&canceled, 1, 7, &cancel, &mut emitter(Budget::new(0, 0).unwrap()), &mut sink).unwrap();
        assert_eq!(receipt.outcome, ObserveOutcome::Canceled);
        assert!(sink.0.iter().all(|(class, _)| *class == DeliveryClass::Terminal));
        assert_eq!(sink.0.len(), 3);
    });
}
