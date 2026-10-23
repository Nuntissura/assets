//! MotionJS adapter contract over the deterministic scripted test double (no JavaScript engine is
//! linked in this slice). Expected values are hand-computed in the comments.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, Ticks};
use hsk_studio_motion::*;
use hsk_studio_motion_js::*;
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort,
};
use std::{
    sync::{Arc, OnceLock},
    thread,
    time::Duration,
};

const S: i64 = 254_016_000_000;

type P = MotionJsProvider<ScriptedEngine>;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 30s deadline: {e}"),
    }
}

/// Host double. Paths: comp/layer/transform/position [960,540]; comp/a|b|c = [2],[3],[4];
/// tick/x = [requested tick]; trip cancels the shared token; amb / cyc return typed host errors;
/// comp/nest evaluates `num:1` through the nested provider while the outer realm is live.
#[derive(Default)]
struct Host {
    token: CancellationToken,
    nest: OnceLock<Arc<P>>,
}

impl HostRead for Host {
    fn read(&self, path: &PropertyPath, tick: EvalTick, fuel: &mut Fuel) -> Result<Vec<f64>, ExprError> {
        fuel.charge(1)?;
        match path.as_str() {
            "comp/layer/transform/position" => Ok(vec![960.0, 540.0]),
            "comp/a" => Ok(vec![2.0]),
            "comp/b" => Ok(vec![3.0]),
            "comp/c" => Ok(vec![4.0]),
            "tick/x" => Ok(vec![tick.value() as f64]),
            "trip" => {
                self.token.cancel();
                Ok(vec![0.0])
            }
            "amb" => Err(ExprError::new(ExprErrorCode::Ambiguous)),
            "cyc" => Err(ExprError::new(ExprErrorCode::DependencyCycle)),
            "comp/nest" => {
                let request = ExprRequest {
                    source: "num:1",
                    revision: 0,
                    tick,
                    seed: 0,
                    dimension: 1,
                    base: None,
                };
                let inner = self.nest.get().expect("nest provider");
                inner.evaluate(&request, &NoHost, &mut Fuel::new(5), &CancellationToken::default())
            }
            _ => Err(ExprError::new(ExprErrorCode::ReferenceMissing)),
        }
    }
}

fn provider() -> P {
    MotionJsProvider::new(ScriptedEngine::new(), Quotas::default())
}

fn expr(dimension: usize, fallback: f64, source: &str) -> Property {
    Property::new("v", vec![fallback; dimension], vec![]).unwrap().with_expression(source).unwrap()
}

fn run_property(provider: &P, mut property: Property, tick: i64, fuel: u64) -> Evaluation {
    let ctx = EvalContext { expected_revision: property.revision(), tick: EvalTick::new(tick), seed: 9 };
    let host = Host::default();
    property
        .evaluate_with(&ctx, provider, &host, &mut Fuel::new(fuel), &CancellationToken::default())
        .unwrap()
}

fn call(
    provider: &P,
    source: &str,
    dimension: usize,
    host: &Host,
    fuel: &mut Fuel,
    cancel: &CancellationToken,
) -> Result<Vec<f64>, ExprError> {
    let request = ExprRequest { source, revision: 0, tick: EvalTick::new(0), seed: 9, dimension, base: None };
    provider.evaluate(&request, host, fuel, cancel)
}

fn eval(provider: &P, source: &str, dimension: usize) -> Result<Vec<f64>, ExprError> {
    let host = Host::default();
    call(provider, source, dimension, &host, &mut Fuel::new(1000), &CancellationToken::default())
}

fn at(line: u32, column: u32) -> Option<SourceSpan> {
    Some(SourceSpan { line, column })
}

fn spec() -> QuotaSpec {
    QuotaSpec {
        max_source_bytes: 64,
        memory_bytes: 1000,
        stack_bytes: 4096,
        recursion_limit: 5,
        max_host_reads: 2,
        deadline: Duration::from_secs(1),
        ..QuotaSpec::default()
    }
}

#[test]
fn provider_evaluates_through_motion_property_and_store() {
    bounded_case("provider_evaluates_through_motion_property_and_store", || {
        let provider = provider().with_date(DateProfile::new(1000, 0).unwrap());
        assert_eq!(provider.engine_identity().name, "scripted-double");

        // Plain value; Motion's own 1 unit pays the provider call.
        let done = run_property(&provider, expr(1, 0.0, "num:3"), 0, 10);
        assert_eq!(done.value, [3.0]);
        assert_eq!((done.receipt.profile, done.receipt.fuel_used, done.receipt.disabled_by_error), (PROFILE_ID, 1, None));
        assert_eq!(done.receipt.state, PropertyState::ExpressionDriven);

        // Host read returns the independently supplied canonical value; Motion 1 + host read 1.
        let done = run_property(&provider, expr(2, 0.0, "read:comp/layer/transform/position"), 0, 10);
        assert_eq!((done.value, done.receipt.fuel_used), (vec![960.0, 540.0], 2));
        // Canonical tick vs explicit tick on the read.
        let done = run_property(&provider, expr(1, 0.0, "read:tick/x"), S / 2, 10);
        assert_eq!(done.value, [127_008_000_000.0]);
        let done = run_property(&provider, expr(1, 0.0, "read:tick/x#254016000000"), S / 2, 10);
        assert_eq!(done.value, [254_016_000_000.0]);

        // Fresh realm per evaluation: a global set by one evaluation does not exist in the next.
        assert_eq!(run_property(&provider, expr(1, 0.0, "set:g=5;num:1"), 0, 10).value, [1.0]);
        let leaked = run_property(&provider, expr(1, 7.0, "get:g"), 0, 10);
        assert_eq!(leaked.value, [7.0]);
        assert_eq!(leaked.receipt.disabled_by_error, Some(ExprErrorCode::ReferenceMissing));

        // Math.random is unavailable: typed unsupported with its span, fallback applied.
        let random = run_property(&provider, expr(1, 7.0, "random"), 0, 10);
        assert_eq!((random.value, random.receipt.disabled_by_error, random.receipt.error_span), (vec![7.0], Some(ExprErrorCode::Unsupported), at(1, 1)));

        // Date comes only from the injected profile: epoch 1000 ms + 1 ms of ticks = 1001.
        assert_eq!(run_property(&provider, expr(1, 0.0, "date"), 254_016_000, 10).value, [1001.0]);
        let edge = MotionJsProvider::new(ScriptedEngine::new(), Quotas::default())
            .with_date(DateProfile::new(MAX_ABS_UTC_MS, 0).unwrap());
        let out_of_range = run_property(&edge, expr(1, 4.0, "date"), 254_016_000, 10);
        assert_eq!((out_of_range.value, out_of_range.receipt.disabled_by_error), (vec![4.0], Some(ExprErrorCode::Unsupported)));

        // Through Motion's PropertyStore: y reads x at the canonical tick (ramp 0->10, midpoint 5);
        // a read edge back to y is a refused cycle naming both paths.
        let ramp = Property::new(
            "x",
            vec![0.0],
            vec![Keyframe::new(Ticks::new(0), vec![0.0]), Keyframe::new(Ticks::new(S as u64), vec![10.0])],
        )
        .unwrap();
        let (x, y) = (PropertyPath::new("c/x").unwrap(), PropertyPath::new("c/y").unwrap());
        let mut store = PropertyStore::new();
        store.insert(x.clone(), ramp).unwrap();
        store.insert(y.clone(), expr(1, 0.0, "read:c/x")).unwrap();
        store.set_reads(&y, std::slice::from_ref(&x)).unwrap();
        assert_eq!(
            store.set_reads(&x, std::slice::from_ref(&y)),
            Err(MotionError::Cycle { participants: vec![x.clone(), y.clone()] })
        );
        let ctx = StoreContext { expected_revision: store.revision(), tick: EvalTick::new(S / 2), seed: 1 };
        let ticked = store.evaluate_tick(&ctx, &provider, &mut Fuel::new(10), &CancellationToken::default()).unwrap();
        assert_eq!(ticked.get(&y).unwrap().value, [5.0]);
        assert_eq!(ticked.get(&y).unwrap().receipt.profile, PROFILE_ID);
    });
}

#[test]
fn quota_fuel_and_deadline_accounting() {
    bounded_case("quota_fuel_and_deadline_accounting", || {
        // Fail closed at configuration: zero and above-ceiling values never become Quotas.
        for (changed, expected) in [
            (QuotaSpec { deadline: Duration::ZERO, ..spec() }, ConfigError::ZeroLimit("deadline")),
            (QuotaSpec { memory_bytes: 0, ..spec() }, ConfigError::ZeroLimit("memory_bytes")),
            (QuotaSpec { stack_bytes: 0, ..spec() }, ConfigError::ZeroLimit("stack_bytes")),
            (QuotaSpec { recursion_limit: 0, ..spec() }, ConfigError::ZeroLimit("recursion_limit")),
            (QuotaSpec { max_concurrent_realms: 0, ..spec() }, ConfigError::ZeroLimit("max_concurrent_realms")),
            (QuotaSpec { stack_bytes: u64::MAX, ..spec() }, ConfigError::AboveCeiling("stack_bytes")),
            (QuotaSpec { recursion_limit: u32::MAX, ..spec() }, ConfigError::AboveCeiling("recursion_limit")),
            (QuotaSpec { deadline: Duration::from_secs(3600), ..spec() }, ConfigError::AboveCeiling("deadline")),
        ] {
            assert_eq!(Quotas::try_from_spec(changed), Err(expected));
        }
        assert!(Quotas::try_from_spec(spec()).is_ok() && Quotas::try_from_spec(QuotaSpec::default()).is_ok());

        let clock = Arc::new(ManualClock::new());
        let provider = MotionJsProvider::new(ScriptedEngine::with_clock(Arc::clone(&clock)), Quotas::try_from_spec(spec()).unwrap())
            .with_clock(clock);
        let (host, cancel) = (Host::default(), CancellationToken::default());
        let budget = Err(ExprError::new(ExprErrorCode::Budget));

        // Fuel: one allowance, every checkpoint charges it; a retry on the SAME Fuel fails at once.
        let mut fuel = Fuel::new(50);
        assert_eq!(call(&provider, "spin", 1, &host, &mut fuel, &cancel), budget);
        assert_eq!((fuel.used(), provider.stats().charged_units, provider.stats().denied(BudgetKind::Fuel)), (50, 50, 1));
        assert_eq!(call(&provider, "spin", 1, &host, &mut fuel, &cancel), budget);
        assert_eq!((provider.stats().charged_units, provider.stats().denied(BudgetKind::Fuel)), (50, 2));

        // Recursion limit 5: depth 6 trips it after 6 charged units; a tiny Fuel trips first.
        let mut fuel = Fuel::new(100);
        assert_eq!(call(&provider, "recurse", 1, &host, &mut fuel, &cancel), budget);
        assert_eq!((fuel.used(), provider.stats().denied(BudgetKind::Recursion)), (6, 1));
        assert_eq!(call(&provider, "recurse", 1, &host, &mut Fuel::new(3), &cancel), budget);
        assert_eq!((provider.stats().denied(BudgetKind::Recursion), provider.stats().denied(BudgetKind::Fuel)), (1, 3));

        // Memory limit 1000 bytes: 1001 denied, exactly 1000 allowed.
        assert_eq!(eval_with(&provider, "alloc:1001"), budget);
        assert_eq!(provider.stats().denied(BudgetKind::Memory), 1);
        assert_eq!(eval_with(&provider, "alloc:1000;num:1"), Ok(vec![1.0]));

        // Deadline 1 s on the injected clock: 500 ms passes, 2000 ms is denied (and is cooperative).
        assert_eq!(eval_with(&provider, "slow:500;num:1"), Ok(vec![1.0]));
        assert_eq!(eval_with(&provider, "slow:2000;num:1"), budget);
        assert_eq!(provider.stats().denied(BudgetKind::Deadline), 1);

        // R7 class "input-caused engine error classified as Internal/poisoning": a malformed command
        // argument is a typed Parse refusal of that call, never an adapter invariant failure.
        let refused = Err(ExprError::at(ExprErrorCode::Parse, 1, 1));
        assert_eq!(eval_with(&provider, "alloc:abc"), refused);
        assert_eq!(eval_with(&provider, "slow:abc"), refused);
        assert!(!provider.is_poisoned());

        // Host reads: nested reads charge the SAME Fuel (host charges 1 each); quota is 2 reads.
        let mut fuel = Fuel::new(10);
        assert_eq!(call(&provider, "readsum:comp/a,comp/b", 1, &host, &mut fuel, &cancel), Ok(vec![5.0]));
        assert_eq!(fuel.used(), 2);
        let mut fuel = Fuel::new(10);
        assert_eq!(call(&provider, "readsum:comp/a,comp/b,comp/c", 1, &host, &mut fuel, &cancel), budget);
        assert_eq!((fuel.used(), provider.stats().denied(BudgetKind::HostReads)), (2, 1));
        // Host fuel exhaustion inside a read maps to a fuel denial, not a host error.
        assert_eq!(call(&provider, "readsum:comp/a,comp/b", 1, &host, &mut Fuel::new(1), &cancel), budget);

        // Source size is checked before any realm exists.
        let started = provider.stats().realms_started;
        let long = format!("num:1;{}", ";".repeat(60));
        assert_eq!(long.len(), 66);
        assert_eq!(eval_with(&provider, &long), budget);
        assert_eq!((provider.stats().realms_started, provider.stats().denied(BudgetKind::SourceBytes)), (started, 1));
        assert!(provider.stats().realms_started == provider.stats().realms_retired);
    });
}

fn eval_with(provider: &P, source: &str) -> Result<Vec<f64>, ExprError> {
    let host = Host::default();
    call(provider, source, 1, &host, &mut Fuel::new(1000), &CancellationToken::default())
}

struct Sink(Vec<DeliveryClass>);
impl SinkPort for Sink {
    fn try_send(&mut self, class: DeliveryClass, _bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(class);
        Ok(())
    }
}

#[test]
fn typed_errors_and_output_rules() {
    bounded_case("typed_errors_and_output_rules", || {
        let provider = provider();
        let code = |source: &str, dimension: usize| eval(&provider, source, dimension);
        let typed = |code: ExprErrorCode, span: Option<SourceSpan>| Err(ExprError { code, span });

        // Engine failures map to Motion's typed codes with exact source positions.
        assert_eq!(code("parse@3:7", 1), typed(ExprErrorCode::Parse, at(3, 7)));
        assert_eq!(code("unsupported@2:1", 1), typed(ExprErrorCode::Unsupported, at(2, 1)));
        assert_eq!(code("throw:ref@4:2", 1), typed(ExprErrorCode::ReferenceMissing, at(4, 2)));
        assert_eq!(code("throw:other@5:9", 1), typed(ExprErrorCode::Unsupported, at(5, 9)));
        assert_eq!(code("get:nothing", 1), typed(ExprErrorCode::ReferenceMissing, None));
        assert_eq!(code("bogus", 1), typed(ExprErrorCode::Parse, at(1, 1)));
        // Host errors keep their exact typed code; an invalid or unknown path is reference_missing.
        assert_eq!(code("read:amb", 1), typed(ExprErrorCode::Ambiguous, None));
        assert_eq!(code("read:cyc", 1), typed(ExprErrorCode::DependencyCycle, None));
        assert_eq!(code("read:a//b", 1), typed(ExprErrorCode::ReferenceMissing, None));
        assert_eq!(code("read:comp/none", 1), typed(ExprErrorCode::ReferenceMissing, None));

        // Output rules: exactly a finite number (dimension 1) or an array of `dimension` finite
        // numbers within node/depth caps; nothing partial ever escapes.
        let output = typed(ExprErrorCode::OutputType, None);
        for (source, dimension) in [
            ("obj", 1), ("nan", 1), ("bool", 1), ("null", 1), ("", 1), ("num:inf", 1),
            ("big:5000", 5000), ("deep:20", 1), ("arr:1,2", 1), ("num:1", 2), ("arr:1,nan", 2),
            ("big:2", 3),
        ] {
            assert_eq!(code(source, dimension), output, "{source}");
        }
        assert_eq!(code("num:3", 1), Ok(vec![3.0]));
        assert_eq!(code("arr:1,2", 2), Ok(vec![1.0, 2.0]));
        assert_eq!(code("big:3", 3), Ok(vec![1.0, 1.0, 1.0]));
        assert_eq!(code("deep:1", 1), Ok(vec![1.0]));
        assert_eq!(code("deep:2", 1), output, "nested array is not a plain numeric array");

        // Cancellation: before evaluation no realm starts; during evaluation the next checkpoint
        // stops it with no further fuel charged.
        let before = provider.stats();
        let token = CancellationToken::default();
        token.cancel();
        let host = Host::default();
        assert_eq!(call(&provider, "num:1", 1, &host, &mut Fuel::new(5), &token), Err(ExprError::canceled()));
        assert_eq!(provider.stats().realms_started, before.realms_started);
        let host = Host::default();
        let tripped = host.token.clone();
        let mut fuel = Fuel::new(100);
        let charged = provider.stats().charged_units;
        assert_eq!(call(&provider, "read:trip;spin", 1, &host, &mut fuel, &tripped), Err(ExprError::canceled()));
        assert_eq!((fuel.used(), provider.stats().charged_units), (1, charged));

        // A panicking engine is contained: typed unsupported, provider poisoned, then fail closed.
        let fragile = self::provider();
        assert_eq!(eval(&fragile, "panic", 1), typed(ExprErrorCode::Unsupported, None));
        assert!(fragile.is_poisoned());
        let started = fragile.stats().realms_started;
        assert_eq!(eval(&fragile, "num:1", 1), typed(ExprErrorCode::Unsupported, None));
        assert_eq!(fragile.stats().realms_started, started);

        // Observability: closed outcomes only.
        let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
        let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
        let emitter = || Observe::new(1, 7, resource.clone(), actor.clone(), Budget::new(0, 0).unwrap());
        let cancel = CancellationToken::default();
        let mut sink = Sink(Vec::new());
        let mut outcomes = Vec::new();
        for result in [Ok(vec![1.0]), code("unsupported@1:1", 1), code("parse@1:1", 1)] {
            outcomes.push(report_eval(&result, 1, 7, &cancel, &mut emitter(), &mut sink).unwrap().outcome);
        }
        let canceled = report_eval(&Err(ExprError::canceled()), 1, 7, &cancel, &mut emitter(), &mut sink).unwrap();
        outcomes.push(canceled.outcome);
        assert_eq!(
            outcomes,
            [Outcome::Success, Outcome::Failure(FailureCode::Unsupported), Outcome::Failure(FailureCode::Validation), Outcome::Canceled]
        );
        assert!(sink.0.iter().all(|class| *class == DeliveryClass::Terminal) && sink.0.len() == 4);
    });
}

#[test]
fn realm_permits_poison_and_retire() {
    bounded_case("realm_permits_poison_and_retire", || {
        // Clean retirement on the owning thread after several evaluations.
        let provider = provider();
        for source in ["num:1", "set:g=1;num:2", "parse@1:1", "spin"] {
            let _ = eval_with(&provider, source);
        }
        let report = provider.retire();
        assert_eq!((report.realms_started, report.realms_retired, report.permits_released, report.owner_thread, report.poisoned), (4, 4, true, true, false));
        assert!(report.is_clean());

        // Retiring on another thread is reported (native runtimes are thread-affine).
        let moved = provider_with_one_run();
        let foreign = thread::spawn(move || moved.retire()).join().unwrap();
        assert!(!foreign.owner_thread && !foreign.is_clean());

        // A realm that is not dropped poisons the provider; retirement reports it unclean.
        let leaky = self::provider();
        assert_eq!(eval_with(&leaky, "leak;num:1"), Err(ExprError::new(ExprErrorCode::Unsupported)));
        assert!(leaky.is_poisoned());
        assert_eq!(eval_with(&leaky, "num:1"), Err(ExprError::new(ExprErrorCode::Unsupported)));
        let report = leaky.retire();
        assert_eq!((report.realms_started, report.realms_retired, report.poisoned), (1, 0, true));
        assert!(!report.is_clean());

        // Permits: with one allowed realm a nested evaluation (a host read that re-enters the
        // provider while the outer realm is live) is denied; with two it is served.
        for (realms, expected) in [(1, Err(ExprError::new(ExprErrorCode::Budget))), (2, Ok(vec![1.0]))] {
            let quotas = Quotas::try_from_spec(QuotaSpec { max_concurrent_realms: realms, ..QuotaSpec::default() }).unwrap();
            let provider = Arc::new(MotionJsProvider::new(ScriptedEngine::new(), quotas));
            let host = Host::default();
            host.nest.set(Arc::clone(&provider)).ok().unwrap();
            let outer = call(&provider, "read:comp/nest", 1, &host, &mut Fuel::new(20), &CancellationToken::default());
            assert_eq!(outer, expected, "realms={realms}");
            let denied = provider.stats().denied(BudgetKind::Realms);
            assert_eq!(denied, if realms == 1 { 1 } else { 0 });
            drop(host);
            let report = Arc::try_unwrap(provider).ok().expect("sole owner").retire();
            assert!(report.permits_released && report.is_clean(), "{report:?}");
        }
    });
}

fn provider_with_one_run() -> P {
    let provider = provider();
    assert_eq!(eval_with(&provider, "num:1"), Ok(vec![1.0]));
    provider
}
