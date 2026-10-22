#![cfg(feature = "quickjs")]
//! QuickJS backend contract (run with `--features quickjs`). Numeric expectations are hand-computed
//! (e.g. sum 0..49999 = 1_249_975_000); policy expectations come from the adoption review controls.
//! Without `test-faults` (product build) the suite runs against the worker process; with `test-faults`
//! it runs against the explicit `unsafe_in_process` engine unless `HSK_MOTION_JS_TEST_BACKEND=worker`
//! selects the worker. The assertions are identical for both backends.
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_motion::*;
use hsk_studio_motion_js::*;
use std::{thread, time::Duration};

const S: i64 = 254_016_000_000;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 60s deadline: {e}"),
    }
}

/// comp/layer/transform/position [960,540]; comp/a=[2]; comp/b=[3]; tick/x=[requested tick];
/// amb -> Ambiguous. Every read charges 1 unit of the shared Fuel.
struct Host;

impl HostRead for Host {
    fn read(&self, path: &PropertyPath, tick: EvalTick, fuel: &mut Fuel) -> Result<Vec<f64>, ExprError> {
        fuel.charge(1)?;
        match path.as_str() {
            "comp/layer/transform/position" => Ok(vec![960.0, 540.0]),
            "comp/a" => Ok(vec![2.0]),
            "comp/b" => Ok(vec![3.0]),
            "tick/x" => Ok(vec![tick.value() as f64]),
            "amb" => Err(ExprError::new(ExprErrorCode::Ambiguous)),
            _ => Err(ExprError::new(ExprErrorCode::ReferenceMissing)),
        }
    }
}

type P = MotionJsProvider<QuickJsBackend>;

fn options() -> QuickJsOptions {
    #[cfg(feature = "test-faults")]
    {
        if std::env::var("HSK_MOTION_JS_TEST_BACKEND").as_deref() != Ok("worker") {
            return QuickJsOptions::unsafe_in_process();
        }
    }
    QuickJsOptions::default().with_worker_program(env!("CARGO_BIN_EXE_motion-js-worker"))
}

fn backend(spec: QuotaSpec) -> Result<QuickJsBackend, QuickJsConfigError> {
    QuickJsBackend::new(&Quotas::try_from_spec(spec).unwrap(), options())
}

fn provider_with(spec: QuotaSpec) -> P {
    let quotas = Quotas::try_from_spec(spec).unwrap();
    // These tests attack the engine's own hardening with hostile source, so they run without the
    // profile validator (tests/profile_contract.rs covers the validator and its precedence).
    MotionJsProvider::new(QuickJsBackend::new(&quotas, options()).unwrap(), quotas).without_validator()
}

fn call(provider: &P, source: &str, dimension: usize, tick: i64, fuel: &mut Fuel, cancel: &CancellationToken) -> Result<Vec<f64>, ExprError> {
    let request = ExprRequest { source, revision: 0, tick: EvalTick::new(tick), seed: 0, dimension, base: None };
    provider.evaluate(&request, &Host, fuel, cancel)
}

fn eval(provider: &P, source: &str, dimension: usize) -> Result<Vec<f64>, ExprError> {
    call(provider, source, dimension, 0, &mut Fuel::new(1000), &CancellationToken::default())
}

fn code(result: Result<Vec<f64>, ExprError>) -> (ExprErrorCode, Option<u32>) {
    let error = result.expect_err("expected a typed rejection");
    (error.code, error.span.map(|s| s.line))
}

#[test]
fn quickjs_evaluates_and_reads_host() {
    bounded_case("quickjs_evaluates_and_reads_host", || {
        let provider = provider_with(QuotaSpec::default()).with_date(DateProfile::new(1000, 0).unwrap());
        let identity = provider.engine_identity();
        assert!(identity.native && identity.name.contains("quickjs"));

        assert_eq!(eval(&provider, "1+2", 1), Ok(vec![3.0]));
        assert_eq!(eval(&provider, "[1,2,3]", 3), Ok(vec![1.0, 2.0, 3.0]));
        assert_eq!(eval(&provider, "var s=0; for(var i=0;i<50000;i++){s+=i}; s", 1), Ok(vec![1_249_975_000.0]));

        // Host read: the independently supplied canonical value; host charged 1, no engine polls.
        let mut fuel = Fuel::new(50);
        let read = call(&provider, "hostRead('comp/layer/transform/position')", 2, 0, &mut fuel, &CancellationToken::default());
        assert_eq!((read, fuel.used()), (Ok(vec![960.0, 540.0]), 1));
        // Two distinct reads = 2 charged units; the same read twice in one run is served from the
        // evaluation-local cache (3 + 2 = 5 and 2 * 2 = 4).
        let before = provider.stats().host_reads;
        let mut fuel = Fuel::new(50);
        let sum = call(&provider, "hostRead('comp/a')[0] + hostRead('comp/b')[0]", 1, 0, &mut fuel, &CancellationToken::default());
        assert_eq!((sum, fuel.used(), provider.stats().host_reads - before), (Ok(vec![5.0]), 2, 2));
        let mut fuel = Fuel::new(50);
        let square = call(&provider, "hostRead('comp/a')[0] * hostRead('comp/a')[0]", 1, 0, &mut fuel, &CancellationToken::default());
        assert_eq!((square, fuel.used(), provider.stats().host_reads - before), (Ok(vec![4.0]), 1, 3));
        // Canonical tick vs explicit tick argument.
        let at = |source: &str| call(&provider, source, 1, S / 2, &mut Fuel::new(50), &CancellationToken::default());
        assert_eq!(at("hostRead('tick/x')[0]"), Ok(vec![127_008_000_000.0]));
        assert_eq!(at("hostRead('tick/x', 254016000000)[0]"), Ok(vec![254_016_000_000.0]));
        // Typed host and argument failures; JS try/catch cannot swallow them.
        assert_eq!(code(eval(&provider, "hostRead('amb')", 1)), (ExprErrorCode::Ambiguous, None));
        assert_eq!(code(eval(&provider, "try { hostRead('amb') } catch (e) { 7 }", 1)), (ExprErrorCode::Ambiguous, None));
        assert_eq!(code(eval(&provider, "hostRead('nowhere')", 1)).0, ExprErrorCode::ReferenceMissing);
        assert_eq!(code(eval(&provider, "hostRead('a//b')", 1)).0, ExprErrorCode::ReferenceMissing);
        assert_eq!(code(eval(&provider, "hostRead('comp/a', 1.5)", 1)).0, ExprErrorCode::ReferenceMissing);

        // Only the injected Date profile exists: epoch 1000 ms + 254_016_000 ticks (1 ms) = 1001.
        assert_eq!(call(&provider, "hostUtcMs()", 1, 254_016_000, &mut Fuel::new(5), &CancellationToken::default()), Ok(vec![1001.0]));

        // Fresh runtime per evaluation: nothing from the previous run exists.
        assert_eq!(eval(&provider, "globalThis.g = 5; 1", 1), Ok(vec![1.0]));
        assert_eq!(eval(&provider, "typeof g === 'undefined' ? 7 : 8", 1), Ok(vec![7.0]));

        // Through Motion: Property (receipt names this provider) and PropertyStore (declared read).
        let mut property = Property::new("v", vec![0.0], vec![]).unwrap().with_expression("hostRead('comp/a')[0] + 1").unwrap();
        let ctx = EvalContext { expected_revision: property.revision(), tick: EvalTick::new(0), seed: 1 };
        let done = property.evaluate_with(&ctx, &provider, &Host, &mut Fuel::new(50), &CancellationToken::default()).unwrap();
        assert_eq!((done.value, done.receipt.profile, done.receipt.fuel_used), (vec![3.0], PROFILE_ID, 2));
        let ramp = Property::new("x", vec![0.0], vec![Keyframe::new(Ticks::new(0), vec![0.0]), Keyframe::new(Ticks::new(S as u64), vec![10.0])]).unwrap();
        let expr = |source: &str| Property::new("v", vec![0.0], vec![]).unwrap().with_expression(source).unwrap();
        let (x, y, z) = (PropertyPath::new("c/x").unwrap(), PropertyPath::new("c/y").unwrap(), PropertyPath::new("c/z").unwrap());
        let mut store = PropertyStore::new();
        store.insert(x.clone(), ramp).unwrap();
        store.insert(y.clone(), expr("hostRead('c/x')[0] * 2")).unwrap();
        store.insert(z.clone(), expr("hostRead('c/y')[0] + hostRead('c/x')[0]")).unwrap();
        store.set_reads(&y, std::slice::from_ref(&x)).unwrap();
        store.set_reads(&z, &[y.clone(), x.clone()]).unwrap();
        let ctx = StoreContext { expected_revision: store.revision(), tick: EvalTick::new(S / 2), seed: 1 };
        let ticked = store.evaluate_tick(&ctx, &provider, &mut Fuel::new(20), &CancellationToken::default()).unwrap();
        // x midpoint 5; y = 2x = 10; z = y + x = 15; fuel y: 1+1, z: 1+2.
        assert_eq!((ticked.get(&x).unwrap().value.clone(), ticked.get(&y).unwrap().value.clone(), ticked.get(&z).unwrap().value.clone()), (vec![5.0], vec![10.0], vec![15.0]));
        assert_eq!((ticked.get(&y).unwrap().receipt.fuel_used, ticked.get(&z).unwrap().receipt.fuel_used), (2, 3));
    });
}

#[test]
fn quickjs_limits_interrupt_and_fuel() {
    bounded_case("quickjs_limits_interrupt_and_fuel", || {
        // Fail-closed configuration: the wrapper maps stacks above 16 MiB to "unlimited".
        let engine = backend;
        let default = QuotaSpec::default();
        assert_eq!(engine(QuotaSpec { stack_bytes: 32 << 20, ..default }).err(), Some(QuickJsConfigError::StackAboveWrapperLimit));
        assert_eq!(engine(QuotaSpec { stack_bytes: 32 << 10, ..default }).err(), Some(QuickJsConfigError::StackBelowMinimum));
        assert_eq!(engine(QuotaSpec { memory_bytes: 512 << 10, ..default }).err(), Some(QuickJsConfigError::MemoryBelowMinimum));
        // Heap limit accepted by the live engine and read back.
        let readback = engine(default).unwrap().verify_limits().unwrap();
        assert_eq!((readback.memory_limit, readback.stack_applied), (16 << 20, 512 << 10));
        let mismatched = MotionJsProvider::new(engine(default).unwrap(), Quotas::try_from_spec(QuotaSpec { memory_bytes: 8 << 20, ..default }).unwrap()).without_validator();
        assert_eq!(code(eval(&mismatched, "1", 1)).0, ExprErrorCode::Unsupported);
        assert!(mismatched.is_poisoned(), "engine/quota mismatch must fail closed");

        let provider = provider_with(QuotaSpec { memory_bytes: 4 << 20, max_host_reads: 2, ..default });
        let cancel = CancellationToken::default();
        // Fuel: one allowance for engine polls; exhaustion is permanent so a retry fails at once.
        let mut fuel = Fuel::new(10);
        assert_eq!(code(call(&provider, "while (true) {}", 1, 0, &mut fuel, &cancel)).0, ExprErrorCode::Budget);
        assert_eq!((fuel.used(), provider.stats().denied(BudgetKind::Fuel)), (10, 1));
        // The next busy loop stops at its first engine poll; a script that never reaches a poll is
        // free at the provider level (Motion's own 1-unit call charge still applies upstream).
        assert_eq!(code(call(&provider, "while (true) {}", 1, 0, &mut fuel, &cancel)).0, ExprErrorCode::Budget);
        assert_eq!((fuel.used(), provider.stats().denied(BudgetKind::Fuel)), (10, 2));
        assert_eq!(call(&provider, "1", 1, 0, &mut fuel, &cancel), Ok(vec![1.0]));
        // A finite loop is charged deterministically: same build + same source = same units.
        let used = |_: ()| {
            let mut fuel = Fuel::new(1000);
            assert_eq!(call(&provider, "var s=0; for(var i=0;i<50000;i++){s+=i}; s", 1, 0, &mut fuel, &cancel), Ok(vec![1_249_975_000.0]));
            fuel.used()
        };
        let (first, second) = (used(()), used(()));
        assert!(first >= 1 && first == second, "{first} {second}");

        // Heap limit (4 MiB) and stack limit (512 KiB) stop the script with a typed rejection, not a
        // crash or a poison, even from a caller thread whose own stack is far smaller (the engine runs
        // on its own sized stack). QuickJS reports both limits as catchable errors a script can forge,
        // so they are thrown errors (Unsupported), never counted budget denials (STUDIO-R5).
        assert_eq!(code(eval(&provider, "'x'.repeat(100000000).length", 1)).0, ExprErrorCode::Unsupported);
        assert_eq!(provider.stats().denied(BudgetKind::Memory), 0);
        let small = thread::Builder::new().stack_size(128 << 10).spawn({
            let provider = provider_with(QuotaSpec::default());
            move || {
                let denied = eval(&provider, "function f() { return f() + 1 } f()", 1);
                (code(denied).0, provider.stats().denied(BudgetKind::Stack), provider.is_poisoned())
            }
        });
        assert_eq!(small.unwrap().join().unwrap(), (ExprErrorCode::Unsupported, 0, false));
        assert!(!provider.is_poisoned());

        // Host-read quota (2): the third distinct read is denied.
        let mut fuel = Fuel::new(10);
        let reads = "hostRead('comp/a'); hostRead('comp/b'); hostRead('tick/x'); 1";
        assert_eq!(code(call(&provider, reads, 1, 0, &mut fuel, &cancel)).0, ExprErrorCode::Budget);
        assert_eq!((provider.stats().denied(BudgetKind::HostReads), fuel.used()), (1, 2));

        // Cooperative deadline (real monotonic clock) and cancellation interrupt a busy loop.
        let quick = provider_with(QuotaSpec { deadline: Duration::from_millis(20), ..default });
        let started = std::time::Instant::now();
        assert_eq!(code(call(&quick, "while (true) {}", 1, 0, &mut Fuel::new(1_000_000_000_000), &cancel)).0, ExprErrorCode::Budget);
        assert_eq!(quick.stats().denied(BudgetKind::Deadline), 1);
        assert!(started.elapsed() < Duration::from_secs(20));
        let slow = provider_with(QuotaSpec { deadline: Duration::from_secs(10), ..default });
        let token = CancellationToken::default();
        let canceller = {
            let token = token.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(30));
                token.cancel();
            })
        };
        assert_eq!(code(call(&slow, "while (true) {}", 1, 0, &mut Fuel::new(1_000_000_000_000), &token)).0, ExprErrorCode::Canceled);
        canceller.join().unwrap();
        assert!(provider.stats().realms_started == provider.stats().realms_retired);
    });
}

#[test]
fn quickjs_denies_dynamic_compile_and_ambient_authority() {
    bounded_case("quickjs_denies_dynamic_compile_and_ambient_authority", || {
        let provider = provider_with(QuotaSpec::default());
        let unsupported = (ExprErrorCode::Unsupported, Some(1));
        // Every dynamic-compilation entry point found in the bundled source (eval and the four
        // js_function_constructor registrations) and every route to them probed here is denied.
        for source in [
            "eval('1')",
            "(0, eval)('1')",
            "globalThis.eval('1')",
            "Function('return 1')()",
            "new Function('return 1')",
            "(() => {}).constructor('return 1')()",
            "[].constructor.constructor('return 1')()",
            "(function* () {}).constructor('yield 1')",
            "Object.getPrototypeOf(function* () {}).constructor('yield 1')",
            "(async function () {}).constructor('return 1')",
            "(async function* () {}).constructor('return 1')",
            "Math.random()",
        ] {
            assert_eq!(code(eval(&provider, source, 1)), unsupported, "{source}");
        }
        // Strict mode, no regexp compiler, no ambient APIs.
        assert_eq!(code(eval(&provider, "with (Math) {}", 1)), (ExprErrorCode::Parse, Some(1)));
        assert_eq!(code(eval(&provider, "/a/.test('a')", 1)), (ExprErrorCode::Parse, Some(1)));
        assert_eq!(code(eval(&provider, "1 +\n2 +\n", 1)), (ExprErrorCode::Parse, Some(3)));
        assert_eq!(code(eval(&provider, "undefinedName", 1)), (ExprErrorCode::ReferenceMissing, Some(1)));
        assert_eq!(code(eval(&provider, "\n\nundefinedName", 1)), (ExprErrorCode::ReferenceMissing, Some(3)));
        assert_eq!(code(eval(&provider, "throw new Error('x')", 1)), unsupported);
        assert_eq!(code(eval(&provider, "throw 5", 1)), (ExprErrorCode::Unsupported, None));
        let absent = "['Date','Promise','Proxy','setTimeout','console','print','require','process','fetch','std','os','performance','gc'].every(n => typeof globalThis[n] === 'undefined') ? 1 : 0";
        assert_eq!(eval(&provider, absent, 1), Ok(vec![1.0]));
        // Async syntax is accepted but inert (no job loop ever runs); it must not leak the realm.
        assert_eq!(eval(&provider, "(async function () { return 1 })().then(function () {}); 2", 1), Ok(vec![2.0]));
        assert_eq!(eval(&provider, "import('x'); 3", 1), Ok(vec![3.0]));

        // Results leave the realm only as a finite number or a flat array of `dimension` numbers;
        // getters, boxed numbers, holes, nesting and cycles never run user code or escape.
        let output = (ExprErrorCode::OutputType, None);
        for (source, dimension) in [
            ("({})", 1),
            ("'s'", 1),
            ("true", 1),
            ("null", 1),
            ("undefined", 1),
            ("NaN", 1),
            ("1n", 1),
            ("new Number(5)", 1),
            ("[1, [2]]", 2),
            ("[new Number(1)]", 1),
            ("new Array(2)", 2),
            ("[1, 2]", 3),
            ("5", 2),
            ("var a = []; a[0] = a; a", 1),
            ("Array.from({ length: 100000 }, () => 1)", 100000),
            ("var a = [1, 2]; Object.defineProperty(a, 0, { get() { throw new Error('getter ran') } }); a", 2),
            ("var a = [1]; Object.defineProperty(a, 0, { get() { return 1 } }); Object.prototype.value = 7; a", 1),
        ] {
            assert_eq!(code(eval(&provider, source, dimension)), output, "{source}");
        }
        assert_eq!(eval(&provider, "Object.prototype.value = 7; var a = [1, 2]; a", 2), Ok(vec![1.0, 2.0]));
        assert_eq!(eval(&provider, "Array.prototype.push = null; Array.isArray = null; [4, 5]", 2), Ok(vec![4.0, 5.0]));
    });
}

#[test]
fn quickjs_realm_retire_and_concurrency() {
    bounded_case("quickjs_realm_retire_and_concurrency", || {
        let provider = provider_with(QuotaSpec { max_concurrent_realms: 4, ..QuotaSpec::default() });
        // Several concurrent evaluations: separate runtimes behind bounded admission, same answer.
        let provider = std::sync::Arc::new(provider);
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let provider = std::sync::Arc::clone(&provider);
                thread::spawn(move || eval(&provider, "var t = 0; for (var i = 1; i <= 100; i++) { t += i } t", 1))
            })
            .collect();
        for worker in workers {
            assert_eq!(worker.join().unwrap(), Ok(vec![5050.0]));
        }
        // Mixed outcomes, including failures, still retire every runtime.
        for source in ["1+2", "while (true) {}", "throw 1", "(", "'x'.repeat(100000000)"] {
            let _ = call(&provider, source, 1, 0, &mut Fuel::new(5), &CancellationToken::default());
        }
        let provider = std::sync::Arc::try_unwrap(provider).ok().expect("sole owner");
        let stats = provider.stats();
        assert_eq!((stats.evaluations, stats.realms_started, stats.realms_retired), (9, 9, 9));
        let report = provider.retire();
        assert!(report.is_clean(), "{report:?}");
    });
}

#[test]
fn quickjs_user_getters_cannot_run_unmetered() {
    bounded_case("quickjs_user_getters_cannot_run_unmetered", || {
        let provider = provider_with(QuotaSpec::default());
        let started = std::time::Instant::now();
        // Error classification used to read name/message/stack through user-controllable accessors
        // with the interrupt handler disarmed; a looping getter therefore hung the evaluation.
        for key in ["name", "message", "stack"] {
            let source = format!("var e = new Error('x'); Object.defineProperty(e, '{key}', {{ get() {{ while (true) {{}} }} }}); throw e");
            assert_eq!(code(eval(&provider, &source, 1)).0, ExprErrorCode::Unsupported, "{source}");
        }
        // Result extraction must not consult accessors planted on Object.prototype either
        // (Object.defineProperty reads the descriptor fields `get`/`set` through the prototype chain).
        let planted = "Object.defineProperty(Object.prototype, 'get', { get() { while (true) {} } }); [1, 2]";
        assert_eq!(eval(&provider, planted, 2), Ok(vec![1.0, 2.0]));
        // Host bridge arguments are not coerced through user toString/valueOf.
        let hostile = "hostRead({ toString() { while (true) {} } })";
        assert_eq!(code(eval(&provider, hostile, 1)).0, ExprErrorCode::Unsupported);
        assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
        assert!(!provider.is_poisoned());
        assert_eq!(eval(&provider, "1+2", 1), Ok(vec![3.0]));
    });
}

/// R5 (SMB-MOTION-JS FAIL_V2, diagnostics/motion-js-classify.md): a script must not be able to report
/// itself as a stack or memory budget denial. QuickJS limit errors are catchable objects identical to
/// user-constructed ones, so no realm-visible text or shape may select a budget class.
#[test]
fn quickjs_forged_budget_messages_are_thrown_not_budget() {
    bounded_case("quickjs_forged_budget_messages_are_thrown_not_budget", || {
        let provider = provider_with(QuotaSpec::default());
        for source in [
            // stack
            "throw new RangeError('Maximum call stack size exceeded')",
            "var e = new RangeError('x'); e.message = 'Maximum call stack size exceeded'; throw e",
            "throw Object.create(RangeError.prototype, { message: { value: 'Maximum call stack size exceeded' } })",
            "try { null.x } catch (e) { Object.setPrototypeOf(e, RangeError.prototype); e.message = 'Maximum call stack size exceeded'; throw e }",
            // memory
            "throw new InternalError('out of memory')",
            "var e = new InternalError('x'); e.message = 'out of memory'; throw e",
            "throw Object.create(InternalError.prototype, { message: { value: 'out of memory' } })",
            // the denial marker of the bootstrap throwers
            "throw new TypeError('motionjs-unavailable: eval')",
        ] {
            assert_eq!(code(eval(&provider, source, 1)).0, ExprErrorCode::Unsupported, "{source}");
        }
        let stats = provider.stats();
        assert_eq!((stats.denied(BudgetKind::Stack), stats.denied(BudgetKind::Memory)), (0, 0), "{stats:?}");
        assert!(!provider.is_poisoned());
        assert_eq!(eval(&provider, "1+2", 1), Ok(vec![3.0]));
    });
}

#[test]
fn quickjs_user_error_names_cannot_poison_provider() {
    bounded_case("quickjs_user_error_names_cannot_poison_provider", || {
        let provider = provider_with(QuotaSpec::default());
        // Names, messages and prototypes of user-thrown errors are user data, never engine signals.
        for source in [
            "var e = new Error('x'); e.name = 'InternalError'; throw e",
            "var e = new Error('x'); e.name = 'InternalError'; e.message = 'interrupted'; throw e",
            "throw new InternalError('boom')",
            "var e = new RangeError('x'); e.name = 'InternalError'; throw e",
        ] {
            assert_eq!(code(eval(&provider, source, 1)).0, ExprErrorCode::Unsupported, "{source}");
            assert!(!provider.is_poisoned(), "poisoned by {source}");
        }
        assert_eq!(eval(&provider, "1+2", 1), Ok(vec![3.0]));
        // A genuine heap-limit hit is a thrown error like the forged ones above (no realm-visible signal
        // tells them apart), and it never poisons the provider either.
        let tight = provider_with(QuotaSpec { memory_bytes: 4 << 20, ..QuotaSpec::default() });
        assert_eq!(code(eval(&tight, "'x'.repeat(100000000).length", 1)).0, ExprErrorCode::Unsupported);
        assert!(!tight.is_poisoned());
        let report = provider.retire();
        assert!(report.is_clean(), "{report:?}");
    });
}

/// STUDIO-R7 pre-review B (SMB-MOTION-JS N1, third report of the class "script input poisons the
/// provider"): a raw U+0000 in the source makes the wrapper fail (`CString::new`, rquickjs-core 0.14.0
/// `src/context/ctx.rs:144` -> `Error::InvalidString`) before the engine runs. That error is caused by
/// the SUBMITTED SOURCE, so it must be a typed refusal of that one call, never `Internal`, which
/// poisons the provider for good. The engine layer is tested without the validator (the validator
/// layer is tests/profile_contract.rs); the NUL span is the validator's span.
#[test]
fn quickjs_nul_in_source_is_a_refusal_not_a_poison() {
    bounded_case("quickjs_nul_in_source_is_a_refusal_not_a_poison", || {
        // (source, line, column of the first raw NUL)
        let cases: &[(&str, u32, u32)] = &[
            ("var s = \"a\0b\"; 1", 1, 11),
            ("1 // \0", 1, 6),
            ("1 /* \0 */", 1, 6),
            ("1 +\r\n\"x\0\"", 2, 3),
            ("'\\\0'", 1, 3),
            ("1 +\0 2", 1, 4),
            ("throw 5; // \0", 1, 13),
        ];
        let provider = provider_with(QuotaSpec::default());
        assert!(provider.validator().is_none());
        for (source, line, column) in cases {
            assert_eq!(
                eval(&provider, source, 1),
                Err(ExprError::at(ExprErrorCode::Parse, *line, *column)),
                "{source:?}"
            );
            assert!(!provider.is_poisoned(), "poisoned by {source:?}");
            assert_eq!(eval(&provider, "1+2", 1), Ok(vec![3.0]), "unusable after {source:?}");
        }
        let stats = provider.stats();
        assert_eq!(stats.denials, [0; BudgetKind::COUNT], "{stats:?}");
        assert_eq!(stats.faults, [0; EngineFault::COUNT], "{stats:?}");
        assert_eq!(stats.validation_rejected, 0, "no validator ran: the engine layer classified");
        assert_eq!(stats.realms_started, stats.realms_retired);
        // In worker mode the refusal travels the wire as a typed Parse failure: the same worker
        // process serves every request (no kill, no crash, no respawn).
        if let Some(worker) = provider.engine().worker_stats() {
            assert_eq!((worker.spawned, worker.crashed, worker.protocol_faults), (1, 0, 0), "{worker:?}");
            assert_eq!((worker.killed_deadline, worker.killed_oversize, worker.killed_canceled), (0, 0, 0));
        }

        // A NUL written as an escape is no NUL character in the source: it evaluates as JavaScript says.
        assert_eq!(eval(&provider, "'a\\0b'.length", 1), Ok(vec![3.0]));
        assert_eq!(eval(&provider, "'a\\x00b'.length", 1), Ok(vec![3.0]));
        assert_eq!(eval(&provider, "'a\\u0000b'.length", 1), Ok(vec![3.0]));
        assert_eq!(eval(&provider, "'\\0'.charCodeAt(0)", 1), Ok(vec![0.0]));
        let report = provider.retire();
        assert!(report.is_clean(), "{report:?}");

        // Default configuration (validator in front): refused before any realm exists, same span.
        let quotas = Quotas::default();
        let validated = MotionJsProvider::new(QuickJsBackend::new(&quotas, options()).unwrap(), quotas);
        assert!(validated.validator().is_some());
        for (index, (source, line, column)) in cases.iter().enumerate() {
            assert_eq!(
                eval(&validated, source, 1),
                Err(ExprError::at(ExprErrorCode::Parse, *line, *column)),
                "{source:?}"
            );
            assert_eq!(validated.stats().validation_rejected, u64::try_from(index + 1).unwrap());
        }
        assert_eq!(validated.stats().realms_started, 0, "the engine saw a NUL-bearing source");
        assert!(!validated.is_poisoned());
        assert_eq!(eval(&validated, "'a\\0b'.length", 1), Ok(vec![3.0]));
        assert!(validated.retire().is_clean());
    });
}

/// R7 carried PLAUSIBLE (response_cap): the worker reply cap assumed a host-read path no longer than the
/// source, but a script builds a longer path at run time. With the smallest output quota the cap is
/// 8 + 4096 bytes, so a 5000-byte path used to be an oversize reply: the worker was killed and the call
/// answered OutputType. The realm now refuses a path above the Motion bound itself (ReferenceMissing,
/// the answer the host gives any invalid path), identically for both backends and without a kill.
#[test]
fn quickjs_overlong_host_read_path_is_a_reference_error_not_an_oversize_kill() {
    bounded_case("quickjs_overlong_host_read_path_is_a_reference_error_not_an_oversize_kill", || {
        let provider = provider_with(QuotaSpec { max_output_nodes: 1, ..QuotaSpec::default() });
        for length in [256_usize, 257, 5000, 100_000] {
            let source = format!("hostRead('a'.repeat({length}))");
            assert!(source.len() < 64, "the path must be built at run time, not written in the source");
            assert_eq!(code(eval(&provider, &source, 1)).0, ExprErrorCode::ReferenceMissing, "{source}");
            assert!(!provider.is_poisoned(), "poisoned by {source}");
        }
        assert_eq!(eval(&provider, "1+2", 1), Ok(vec![3.0]));
        if let Some(worker) = provider.engine().worker_stats() {
            assert_eq!((worker.spawned, worker.killed_oversize, worker.crashed), (1, 0, 0), "{worker:?}");
        }
        assert!(provider.retire().is_clean());
    });
}

/// Lane SMB-AI-AE-ORACLE-FOLLOWUPS slice 4: After Effects globals (`AE_GLOBALS_PRELUDE`). Hand values:
/// base 45 at tick 1 s: `value*2` = 90, `value + time*10` = 55; `linear(5, 0, 2, 0, 180)` clamps to
/// 180 and `linear(-1, ...)` to 0; `linear(0.25, [0, 10], [4, 20])` = [1, 12.5] (3-argument form,
/// per component). Without a base (`value` undefined) the result is not a number: OutputType.
/// User spans stay on the user's lines behind the prelude.
#[test]
fn quickjs_binds_after_effects_globals() {
    bounded_case("quickjs_binds_after_effects_globals", || {
        let provider = provider_with(QuotaSpec::default());
        let with_base = |source: &str, base: &[f64]| {
            let request = ExprRequest { source, revision: 0, tick: EvalTick::new(S), seed: 0, dimension: base.len(), base: Some(base) };
            provider.evaluate(&request, &Host, &mut Fuel::new(1000), &CancellationToken::default())
        };
        assert_eq!(with_base("value*2", &[45.0]), Ok(vec![90.0]));
        assert_eq!(with_base("value + time*10", &[45.0]), Ok(vec![55.0]));
        assert_eq!(with_base("[value[1], value[0]]", &[1.5, -2.0]), Ok(vec![-2.0, 1.5]));
        assert_eq!(with_base("linear(5, 0, 2, 0, 180)", &[0.0]), Ok(vec![180.0]));
        assert_eq!(with_base("linear(-1, 0, 2, 0, 180)", &[0.0]), Ok(vec![0.0]));
        assert_eq!(with_base("linear(0.25, [0, 10], [4, 20])", &[0.0, 0.0]), Ok(vec![1.0, 12.5]));
        assert_eq!(code(eval(&provider, "value", 1)), (ExprErrorCode::OutputType, None));
        assert_eq!(code(with_base("value +", &[1.0])), (ExprErrorCode::Parse, Some(1)));
        assert_eq!(code(with_base("\nmissingName", &[1.0])), (ExprErrorCode::ReferenceMissing, Some(2)));
    });
}
