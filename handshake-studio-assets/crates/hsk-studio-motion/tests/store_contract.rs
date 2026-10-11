//! PropertyStore contract: dependency-ordered evaluation, cross-property reads through the
//! evaluated-value `HostRead`, revision/cycle refusal, failure isolation. Expected values are
//! hand-computed in the comments, independent of the implementation.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_motion::*;

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

/// Sources: `sum:<p>,<p>` | `base+sum:<p>,<p>` | `at:<tick>:<p>` | `fail:` | `huge:` | `cancel:`.
struct Reader;

impl ExpressionProvider for Reader {
    fn profile_id(&self) -> &'static str {
        "reader"
    }

    fn evaluate(
        &self,
        request: &ExprRequest<'_>,
        host: &dyn HostRead,
        fuel: &mut Fuel,
        cancel: &CancellationToken,
    ) -> Result<Vec<f64>, ExprError> {
        let (op, rest) = request.source.split_once(':').expect("op");
        let read = |path: &str, tick: EvalTick, fuel: &mut Fuel| {
            host.read(&PropertyPath::new(path).unwrap(), tick, fuel)
                .map(|value| value[0])
        };
        match op {
            "sum" | "base+sum" => {
                let mut total = if op == "base+sum" { request.base.expect("base")[0] } else { 0.0 };
                for path in rest.split(',') {
                    total += read(path, request.tick, fuel)?;
                }
                Ok(vec![total])
            }
            "at" => {
                let (tick, path) = rest.split_once(':').unwrap();
                Ok(vec![read(path, EvalTick::new(tick.parse().unwrap()), fuel)?])
            }
            "fail" => Err(ExprError::at(ExprErrorCode::Parse, 1, 1)),
            "huge" => Ok(vec![1e9]),
            "cancel" => {
                cancel.cancel();
                Err(ExprError::canceled())
            }
            other => panic!("unknown op {other}"),
        }
    }
}

fn p(text: &str) -> PropertyPath {
    PropertyPath::new(text).unwrap()
}

fn ramp(to: f64) -> Property {
    Property::new(
        "v",
        vec![0.0],
        vec![
            Keyframe::new(Ticks::new(0), vec![0.0]),
            Keyframe::new(Ticks::new(S as u64), vec![to]),
        ],
    )
    .unwrap()
}

fn fixed(value: f64, expression: Option<&str>) -> Property {
    let property = Property::new("v", vec![value], vec![]).unwrap();
    match expression {
        Some(source) => property.with_expression(source).unwrap(),
        None => property,
    }
}

fn eval(store: &mut PropertyStore, tick: i64, seed: u64, fuel: &mut Fuel) -> Result<TickEvaluation, MotionError> {
    let ctx = StoreContext { expected_revision: store.revision(), tick: EvalTick::new(tick), seed };
    store.evaluate_tick(&ctx, &Reader, fuel, &CancellationToken::default())
}

fn value(evaluation: &TickEvaluation, path: &str) -> Vec<f64> {
    evaluation.get(&p(path)).unwrap().value.clone()
}

fn wired(entries: Vec<(&str, Property, Vec<&str>)>) -> PropertyStore {
    let mut store = PropertyStore::new();
    for (path, property, _) in &entries {
        store.insert(p(path), property.clone()).unwrap();
    }
    for (path, _, reads) in &entries {
        if !reads.is_empty() {
            let reads: Vec<PropertyPath> = reads.iter().map(|r| p(r)).collect();
            store.set_reads(&p(path), &reads).unwrap();
        }
    }
    store
}

#[test]
fn store_orders_and_serves_cross_property_reads() {
    bounded_case("store_orders_and_serves_cross_property_reads", || {
        let build = || {
            wired(vec![
                ("c/l/u", fixed(2.0, Some("sum:c/l/x")), vec![]), // read not declared
                ("c/l/v", fixed(0.0, Some("at:254016000000:c/l/x")), vec!["c/l/x"]),
                ("c/l/v2", fixed(-1.0, Some("at:254016000000:c/l/y")), vec!["c/l/y"]),
                ("c/l/w", fixed(7.0, None), vec![]),
                ("c/l/x", ramp(10.0), vec![]),
                ("c/l/y", fixed(1.0, Some("sum:c/l/x")), vec!["c/l/x"]),
                ("c/l/z", ramp(4.0).with_expression("base+sum:c/l/y,c/l/x").unwrap(), vec!["c/l/y", "c/l/x"]),
            ])
        };
        let mut store = build();
        let mut fuel = Fuel::new(100);
        let done = eval(&mut store, S / 2, 3, &mut fuel).unwrap();

        // Dependencies first, ties by smallest path: ready {u,w,x} -> u, w, x; then v, y; v2, z.
        let order: Vec<&str> = done.results.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(order, ["c/l/u", "c/l/w", "c/l/x", "c/l/v", "c/l/y", "c/l/v2", "c/l/z"]);
        // x: linear 0->10, midpoint 5. y reads x at the canonical tick: 5. z: base 2 (0->4 midpoint)
        // + y 5 + x 5 = 12. w untouched static 7.
        assert_eq!(value(&done, "c/l/x"), [5.0]);
        assert_eq!(value(&done, "c/l/y"), [5.0]);
        assert_eq!(value(&done, "c/l/z"), [12.0]);
        assert_eq!(value(&done, "c/l/w"), [7.0]);
        // v reads x at ANOTHER tick (1 s): x has no expression, so its keyframed value 10.
        assert_eq!(value(&done, "c/l/v"), [10.0]);
        // v2 reads y at another tick: y's expression is active -> unsupported, disabled, static -1.
        let v2 = done.get(&p("c/l/v2")).unwrap();
        assert_eq!(v2.value, [-1.0]);
        assert_eq!((v2.receipt.disabled_by_error, v2.receipt.state), (Some(ExprErrorCode::Unsupported), PropertyState::Static));
        // u reads an undeclared path: reference_missing, disabled, static 2 (read still paid fuel).
        let u = done.get(&p("c/l/u")).unwrap();
        assert_eq!((u.value.clone(), u.receipt.disabled_by_error), (vec![2.0], Some(ExprErrorCode::ReferenceMissing)));
        // Fuel: one unit per provider call + one per host read: u 2, v 2, y 2, v2 2, z 3 = 11.
        let spent: Vec<u64> = ["c/l/u", "c/l/w", "c/l/x", "c/l/v", "c/l/y", "c/l/v2", "c/l/z"]
            .iter()
            .map(|path| done.get(&p(path)).unwrap().receipt.fuel_used)
            .collect();
        assert_eq!(spent, [2, 0, 0, 2, 2, 2, 3]);
        assert_eq!(fuel.used(), 11);
        assert_eq!((done.revision, done.tick, done.seed), (store.revision(), EvalTick::new(S / 2), 3));
        assert_eq!(done.get(&p("c/l/z")).unwrap().receipt.seed, 3);
        assert_eq!(done.get(&p("c/l/z")).unwrap().receipt.state, PropertyState::ExpressionOverKeyframes);

        // Determinism: a fresh identical store gives an identical result; a re-run on the same
        // store (u and v2 are now disabled) gives the same values.
        let again = eval(&mut build(), S / 2, 3, &mut Fuel::new(100)).unwrap();
        assert_eq!(again, done);
        let rerun = eval(&mut store, S / 2, 3, &mut Fuel::new(100)).unwrap();
        for (path, evaluation) in &done.results {
            assert_eq!(rerun.get(path).unwrap().value, evaluation.value, "{path}");
        }
        // A different tick moves only the time-dependent values (tick 0: x 0, y 0, z 0+0+0).
        let start = eval(&mut store, 0, 3, &mut Fuel::new(100)).unwrap();
        assert_eq!((value(&start, "c/l/x"), value(&start, "c/l/y"), value(&start, "c/l/z")), (vec![0.0], vec![0.0], vec![0.0]));
        assert_eq!(value(&start, "c/l/w"), [7.0]);
    });
}

#[test]
fn store_revisions_cycles_and_unknowns() {
    bounded_case("store_revisions_cycles_and_unknowns", || {
        let mut store = PropertyStore::new();
        assert_eq!(store.revision(), 0);
        for name in ["s/a", "s/b", "s/c"] {
            store.insert(p(name), fixed(1.0, None)).unwrap();
        }
        assert_eq!((store.revision(), store.len()), (3, 3));
        store.set_reads(&p("s/a"), &[p("s/b")]).unwrap();
        assert_eq!(store.revision(), 4);
        // Refusals leave the store and its revision untouched.
        assert_eq!(
            store.set_reads(&p("s/b"), &[p("s/a")]),
            Err(MotionError::Cycle { participants: vec![p("s/b"), p("s/a")] })
        );
        assert_eq!(store.insert(p("s/a"), fixed(1.0, None)), Err(MotionError::DuplicateProperty));
        assert_eq!(
            store.set_reads(&p("s/a"), &[p("s/zz")]),
            Err(MotionError::ReferenceMissing { reference: "s/zz".to_owned() })
        );
        assert_eq!(store.update(&p("s/zz"), |_| Ok(())), Err(MotionError::UnknownProperty));
        assert_eq!(
            store.update(&p("s/a"), |property| property.set_static_value(vec![1.0, 2.0])),
            Err(MotionError::ComponentMismatch)
        );
        assert_eq!((store.revision(), store.graph().reads_of(&p("s/b")).len()), (4, 0));
        // An accepted property mutation advances the store revision.
        store.update(&p("s/a"), |property| property.set_static_value(vec![9.0])).unwrap();
        assert_eq!(store.revision(), 5);

        // Stale caller revision is rejected before any evaluation.
        let stale = StoreContext { expected_revision: 4, tick: EvalTick::new(0), seed: 0 };
        let token = CancellationToken::default();
        assert_eq!(
            store.evaluate_tick(&stale, &NoProvider, &mut Fuel::new(0), &token).map(|_| ()),
            Err(MotionError::StaleRevision { expected: 4, actual: 5 })
        );
        // Current revision: static properties need no fuel; order b, a (a reads b), c.
        let fresh = StoreContext { expected_revision: 5, tick: EvalTick::new(0), seed: 0 };
        let done = store.evaluate_tick(&fresh, &NoProvider, &mut Fuel::new(0), &token).unwrap();
        let order: Vec<&str> = done.results.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(order, ["s/b", "s/a", "s/c"]);
        assert_eq!((value(&done, "s/a"), value(&done, "s/b")), (vec![9.0], vec![1.0]));
        // Cancellation is rejected up front; nothing is lost.
        let canceled = CancellationToken::default();
        canceled.cancel();
        assert_eq!(
            store.evaluate_tick(&fresh, &NoProvider, &mut Fuel::new(0), &canceled).map(|_| ()),
            Err(MotionError::Canceled)
        );
        assert_eq!((store.len(), store.revision()), (3, 5));
    });
}

#[test]
fn store_failure_isolation_and_fallback() {
    bounded_case("store_failure_isolation_and_fallback", || {
        // b's expression fails -> disabled, fallback base 2 (0->4 midpoint); a reads b and sees 2.
        let entries = |with_c: bool| {
            let mut entries = vec![
                ("p/a", fixed(0.0, Some("sum:p/b")), vec!["p/b"]),
                ("p/b", ramp(4.0).with_expression("fail:").unwrap(), vec![]),
            ];
            if with_c {
                let bounded = Property::new("v", vec![0.5], vec![]).unwrap().with_bounds(Some(0.0), Some(1.0)).unwrap();
                entries.push(("p/c", bounded.with_expression("huge:").unwrap(), vec![]));
            }
            entries
        };
        let mut healthy = wired(entries(false));
        let done = eval(&mut healthy, S / 2, 0, &mut Fuel::new(10)).unwrap();
        assert_eq!((value(&done, "p/b"), value(&done, "p/a")), (vec![2.0], vec![2.0]));
        let b = done.get(&p("p/b")).unwrap();
        assert_eq!((b.receipt.disabled_by_error, b.receipt.error_span, b.receipt.state), (Some(ExprErrorCode::Parse), Some(SourceSpan { line: 1, column: 1 }), PropertyState::Keyframed));

        // c's expression output violates its hard bounds: the tick aborts with no partial result,
        // every property is still in the store, and a fix plus the new revision succeeds.
        let mut store = wired(entries(true));
        assert_eq!(eval(&mut store, S / 2, 0, &mut Fuel::new(10)).map(|_| ()), Err(MotionError::OutOfBounds));
        assert_eq!(store.len(), 3);
        assert!(["p/a", "p/b", "p/c"].iter().all(|path| store.get(&p(path)).is_some()));
        assert_eq!(store.get(&p("p/c")).unwrap().disabled_by_error(), None);
        store.update(&p("p/c"), |property| property.set_expression(None)).unwrap();
        let fixed_tick = eval(&mut store, S / 2, 0, &mut Fuel::new(10)).unwrap();
        assert_eq!((value(&fixed_tick, "p/c"), value(&fixed_tick, "p/a")), (vec![0.5], vec![2.0]));

        // Provider-signalled cancellation aborts, never disables, never drops the property.
        let mut cancelling = wired(vec![("q/a", fixed(1.0, Some("cancel:")), vec![])]);
        let ctx = StoreContext { expected_revision: cancelling.revision(), tick: EvalTick::new(0), seed: 0 };
        let token = CancellationToken::default();
        assert_eq!(cancelling.evaluate_tick(&ctx, &Reader, &mut Fuel::new(5), &token).map(|_| ()), Err(MotionError::Canceled));
        assert_eq!((cancelling.len(), cancelling.get(&p("q/a")).unwrap().disabled_by_error()), (1, None));

        // One Fuel is shared by every property in the tick. x ramp 0->10, y = x (cost 2), z reads
        // y then x (cost 3): with a limit of 3, y spends 2, z pays its call (3) then the read of y
        // exhausts the allowance -> z disabled by budget with fuel_used 1 and its static fallback.
        let mut shared = wired(vec![
            ("r/x", ramp(10.0), vec![]),
            ("r/y", fixed(1.0, Some("sum:r/x")), vec!["r/x"]),
            ("r/z", fixed(-3.0, Some("sum:r/y,r/x")), vec!["r/y", "r/x"]),
        ]);
        let mut fuel = Fuel::new(3);
        let done = eval(&mut shared, S / 2, 0, &mut fuel).unwrap();
        assert_eq!(value(&done, "r/y"), [5.0]);
        let z = done.get(&p("r/z")).unwrap();
        assert_eq!((z.value.clone(), z.receipt.disabled_by_error, z.receipt.fuel_used), (vec![-3.0], Some(ExprErrorCode::Budget), 1));
        assert_eq!((fuel.used(), fuel.remaining()), (3, 0));
    });
}
