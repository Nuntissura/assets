#![cfg(feature = "quickjs")]
//! Worker-process isolation contract (run with `--features quickjs`; the `motion-js-worker` binary
//! is built by cargo for this test). Numeric expectations are hand-computed (1+2=3, sum 0..49999 =
//! 1_249_975_000, sum 1..100 = 5050); isolation expectations come from the lane task: crash ->
//! typed `engine_crashed` with the test process and provider alive, wall-clock kill, bounded respawn.
use hsk_studio_accord::CancellationToken;
use hsk_studio_motion::*;
use hsk_studio_motion_js::*;
use std::{
    thread,
    time::{Duration, Instant},
};

const WORKER: &str = env!("CARGO_BIN_EXE_motion-js-worker");

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 120s deadline: {e}"),
    }
}

/// comp/a=[2]; comp/b=[3]; tick/x=[requested tick]; amb -> Ambiguous. Every read charges 1 unit.
struct Host;

impl HostRead for Host {
    fn read(
        &self,
        path: &PropertyPath,
        tick: EvalTick,
        fuel: &mut Fuel,
    ) -> Result<Vec<f64>, ExprError> {
        fuel.charge(1)?;
        match path.as_str() {
            "comp/a" => Ok(vec![2.0]),
            "comp/b" => Ok(vec![3.0]),
            "tick/x" => Ok(vec![tick.value() as f64]),
            "amb" => Err(ExprError::new(ExprErrorCode::Ambiguous)),
            _ => Err(ExprError::new(ExprErrorCode::ReferenceMissing)),
        }
    }
}

type P = MotionJsProvider<QuickJsBackend>;

fn provider(spec: QuotaSpec, options: QuickJsOptions) -> P {
    let quotas = Quotas::try_from_spec(spec).unwrap();
    // Hostile source must reach the engine and the worker supervisor, so no profile validator here.
    MotionJsProvider::new(QuickJsBackend::new(&quotas, options).unwrap(), quotas)
        .without_validator()
        .with_date(DateProfile::new(1000, 0).unwrap())
}

fn worker_options(fault: WorkerTestFault, max_spawns: u32) -> QuickJsOptions {
    let mut options = QuickJsOptions::default().with_worker_program(WORKER);
    options.worker.test_fault = fault;
    options.worker.max_spawns = max_spawns;
    options.worker.spawn_window = Duration::from_secs(600);
    options
}

fn run(
    p: &P,
    source: &str,
    dimension: usize,
    fuel: u64,
    tick: i64,
    cancel: &CancellationToken,
) -> (Result<Vec<f64>, ExprError>, u64) {
    let request = ExprRequest {
        source,
        revision: 0,
        tick: EvalTick::new(tick),
        seed: 0,
        dimension,
        base: None,
    };
    let mut fuel = Fuel::new(fuel);
    let result = p.evaluate(&request, &Host, &mut fuel, cancel);
    (result, fuel.used())
}

fn eval(p: &P, source: &str, dimension: usize) -> Result<Vec<f64>, ExprError> {
    run(p, source, dimension, 1000, 0, &CancellationToken::default()).0
}

fn code(result: Result<Vec<f64>, ExprError>) -> ExprErrorCode {
    result.expect_err("expected a typed rejection").code
}

#[test]
fn worker_results_match_in_process_exactly() {
    bounded_case("worker_results_match_in_process_exactly", || {
        let spec = QuotaSpec {
            memory_bytes: 4 << 20,
            max_host_reads: 3,
            max_concurrent_realms: 2,
            ..QuotaSpec::default()
        };
        let inproc = provider(spec, QuickJsOptions::unsafe_in_process());
        let worker = provider(spec, worker_options(WorkerTestFault::None, 4));
        assert!(
            worker.engine_identity().name.contains("worker process")
                && worker.engine_identity().native
        );

        // (source, dimension, fuel, tick): values, spans, typed failures and fuel must be identical.
        let cases: &[(&str, usize, u64, i64)] = &[
            ("1+2", 1, 1000, 0),
            ("[1,2,3]", 3, 1000, 0),
            ("var s=0; for(var i=0;i<50000;i++){s+=i}; s", 1, 1000, 0),
            ("hostRead('comp/a')[0] + hostRead('comp/b')[0]", 1, 50, 0),
            ("hostRead('comp/a')[0] * hostRead('comp/a')[0]", 1, 50, 0),
            ("hostRead('tick/x')[0]", 1, 50, 127_008_000_000),
            ("hostRead('tick/x', 254016000000)[0]", 1, 50, 0),
            ("hostUtcMs()", 1, 5, 254_016_000),
            ("hostRead('amb')", 1, 50, 0),
            ("try { hostRead('amb') } catch (e) { 7 }", 1, 50, 0),
            ("hostRead('nowhere')", 1, 50, 0),
            ("hostRead('comp/a', 1.5)", 1, 50, 0),
            (
                "hostRead('comp/a'); hostRead('comp/b'); hostRead('tick/x'); hostRead('amb'); 1",
                1,
                50,
                0,
            ),
            ("while (true) {}", 1, 10, 0),
            ("'x'.repeat(100000000).length", 1, 1000, 0),
            ("function f() { return f() + 1 } f()", 1, 1000, 0),
            ("eval('1')", 1, 1000, 0),
            ("new Function('return 1')", 1, 1000, 0),
            (
                "(async function* () {}).constructor('return 1')",
                1,
                1000,
                0,
            ),
            ("Math.random()", 1, 1000, 0),
            ("1 +\n2 +\n", 1, 1000, 0),
            ("\n\nundefinedName", 1, 1000, 0),
            ("throw new Error('x')", 1, 1000, 0),
            ("throw 5", 1, 1000, 0),
            (
                "var e = new Error('x'); e.name = 'InternalError'; throw e",
                1,
                1000,
                0,
            ),
            (
                "(async function () { return 1 })().then(function () {}); 2",
                1,
                1000,
                0,
            ),
            ("({})", 1, 1000, 0),
            ("[1, [2]]", 2, 1000, 0),
            ("new Array(2)", 2, 1000, 0),
            ("[1, 2]", 3, 1000, 0),
            (
                "var a = [1, 2]; Object.defineProperty(a, 0, { get() { throw new Error('getter ran') } }); a",
                2,
                1000,
                0,
            ),
            ("globalThis.g = 5; 1", 1, 1000, 0),
            ("typeof g === 'undefined' ? 7 : 8", 1, 1000, 0),
        ];
        let cancel = CancellationToken::default();
        for &(source, dimension, fuel, tick) in cases {
            let local = run(&inproc, source, dimension, fuel, tick, &cancel);
            let remote = run(&worker, source, dimension, fuel, tick, &cancel);
            assert_eq!(remote, local, "{source}");
        }
        // Same provider counters: evaluations, realms, checkpoints, charged units, host reads, denials.
        assert_eq!(worker.stats(), inproc.stats());
        // Independently computed oracle values (not derived from either provider).
        let oracle = |source: &str, dimension| eval(&worker, source, dimension);
        assert_eq!(oracle("1+2", 1), Ok(vec![3.0]));
        assert_eq!(
            oracle("var s=0; for(var i=0;i<50000;i++){s+=i}; s", 1),
            Ok(vec![1_249_975_000.0])
        );
        assert_eq!(
            oracle("hostRead('comp/a')[0] + hostRead('comp/b')[0]", 1),
            Ok(vec![5.0])
        );
        assert_eq!(oracle("hostUtcMs()", 1), Ok(vec![1000.0]));
        assert_eq!(
            code(oracle(
                "

undefinedName",
                1
            )),
            ExprErrorCode::ReferenceMissing
        );
        // One worker process served the whole batch (reuse), nothing was killed or crashed, and the
        // heap/stack limits are read back from inside the worker.
        let stats = worker_stats(&worker);
        assert_eq!(
            (
                stats.spawned,
                stats.crashed,
                stats.killed_deadline,
                stats.killed_canceled
            ),
            (1, 0, 0, 0)
        );
        let readback = worker.engine().verify_limits().unwrap();
        assert_eq!(
            (readback.memory_limit, readback.stack_applied),
            (4 << 20, 512 << 10)
        );
        assert_eq!(
            code(eval(&worker, "while (true) {}", 1)),
            ExprErrorCode::Budget
        );
        assert_eq!(worker_stats(&worker).spawned, 1);
        assert!(worker.retire().is_clean());
    });
}

fn worker_stats(p: &P) -> WorkerStats {
    p.engine().worker_stats().expect("worker backend")
}

#[test]
fn worker_crash_is_typed_contained_and_respawn_is_bounded() {
    bounded_case(
        "worker_crash_is_typed_contained_and_respawn_is_bounded",
        || {
            assert_eq!(EngineFault::Crashed.code(), "engine_crashed");
            let crashed = |p: &P, source: &str, expect_crashes: u64, exit: Option<i32>| {
                assert_eq!(
                    code(eval(p, source, 1)),
                    ExprErrorCode::Unsupported,
                    "{source}"
                );
                let (stats, workers) = (p.stats(), worker_stats(p));
                assert_eq!(
                    (stats.faulted(EngineFault::Crashed), workers.crashed),
                    (expect_crashes, expect_crashes)
                );
                match exit {
                    Some(code) => assert_eq!(workers.last_crash_exit_code, Some(code)),
                    None => assert!(
                        matches!(workers.last_crash_exit_code, Some(c) if c != 0),
                        "{workers:?}"
                    ),
                }
                // The realm died with its process (reaped): retired, not leaked; the provider is healthy.
                assert!(!p.is_poisoned());
                assert_eq!(stats.realms_started, stats.realms_retired);
            };

            // The slice-2 reproducer: with the unmitigated async context the QuickJS GC assert aborts
            // the worker process. The evaluation fails typed, the test process survives, and the next
            // call runs on a respawned worker. The worker hardens itself at start (SetErrorMode and
            // _set_abort_behavior, see os_caps), so abort() ends the process at once with exit code 3
            // instead of being held by the platform crash reporter for 2 s to past 10 s (the R5 flaky
            // finding). The assertion is strict: engine_crashed with exit code 3, well before the 10 s
            // deadline (kept at the ceiling only so a regression shows up as a clear failure).
            let aborting = provider(
                QuotaSpec {
                    deadline: Duration::from_secs(10),
                    ..QuotaSpec::default()
                },
                worker_options(WorkerTestFault::AsyncAbort, 4),
            );
            assert_eq!(eval(&aborting, "1+2", 1), Ok(vec![3.0]));
            let started = Instant::now();
            crashed(&aborting, "async function f(){} 1", 1, Some(3));
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "an aborted worker must end at once, took {:?}",
                started.elapsed()
            );
            assert_eq!(
                eval(
                    &aborting,
                    "var t = 0; for (var i = 1; i <= 100; i++) { t += i } t",
                    1
                ),
                Ok(vec![5050.0])
            );
            assert_eq!(worker_stats(&aborting).spawned, 2);
            assert!(aborting.retire().is_clean());

            // Respawn rate bound (3 spawns per window) with a crash class that exits at once (86).
            let p = provider(
                QuotaSpec::default(),
                worker_options(WorkerTestFault::ExitMidRequest, 3),
            );
            crashed(&p, "exit_now", 1, Some(86));
            assert_eq!(eval(&p, "1+2", 1), Ok(vec![3.0]));
            crashed(&p, "exit_now", 2, Some(86));
            assert_eq!(eval(&p, "1+2", 1), Ok(vec![3.0]));
            crashed(&p, "exit_now", 3, Some(86));
            assert_eq!(worker_stats(&p).spawned, 3);
            // The fourth spawn is refused with a typed fault and no process is started.
            assert_eq!(code(eval(&p, "1+2", 1)), ExprErrorCode::Unsupported);
            let workers = worker_stats(&p);
            assert_eq!(
                (
                    p.stats().faulted(EngineFault::RespawnLimited),
                    workers.spawn_denied,
                    workers.spawned
                ),
                (1, 1, 3)
            );
            assert!(!p.is_poisoned());
            assert!(p.retire().is_clean());
        },
    );
}

#[test]
fn worker_is_killed_at_wall_deadline_and_on_cancel() {
    bounded_case("worker_is_killed_at_wall_deadline_and_on_cancel", || {
        // DeafInterrupt: the engine never polls, like a script stuck in a long C builtin; only the
        // host-side process kill can stop it.
        let options = worker_options(WorkerTestFault::DeafInterrupt, 8);
        let quick = provider(
            QuotaSpec {
                deadline: Duration::from_millis(200),
                ..QuotaSpec::default()
            },
            options.clone(),
        );
        let none = CancellationToken::default();
        let started = Instant::now();
        let (result, used) = run(&quick, "while (true) {}", 1, 1_000_000_000_000, 0, &none);
        let elapsed = started.elapsed();
        assert_eq!(code(result), ExprErrorCode::Budget);
        assert_eq!(used, 0, "a killed worker reports no polls");
        assert!(
            elapsed >= Duration::from_millis(200) && elapsed < Duration::from_secs(10),
            "{elapsed:?}"
        );
        let stats = worker_stats(&quick);
        assert_eq!(
            (
                quick.stats().denied(BudgetKind::Deadline),
                stats.killed_deadline,
                stats.crashed
            ),
            (1, 1, 0)
        );
        // The provider stays usable on a fresh worker.
        assert_eq!(eval(&quick, "1+2", 1), Ok(vec![3.0]));
        assert_eq!(worker_stats(&quick).spawned, 2);
        assert!(quick.retire().is_clean());

        // Cancellation kills the worker too and reports Canceled, not a budget.
        let slow = provider(
            QuotaSpec {
                deadline: Duration::from_secs(10),
                ..QuotaSpec::default()
            },
            options,
        );
        let token = CancellationToken::default();
        let canceller = {
            let token = token.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(100));
                token.cancel();
            })
        };
        let started = Instant::now();
        let (result, _) = run(&slow, "while (true) {}", 1, 1_000_000_000_000, 0, &token);
        canceller.join().unwrap();
        assert_eq!(code(result), ExprErrorCode::Canceled);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(
            (
                worker_stats(&slow).killed_canceled,
                worker_stats(&slow).killed_deadline
            ),
            (1, 0)
        );
        assert_eq!(eval(&slow, "[1,2,3]", 3), Ok(vec![1.0, 2.0, 3.0]));
        assert!(slow.retire().is_clean());
    });
}

/// R7 carried PLAUSIBLE (uncancellable HELLO wait): the host waited for a new worker's HELLO for the
/// whole `spawn_timeout` without looking at the cancel token. A worker that stalls before its HELLO
/// (here 60 s, with a 30 s spawn timeout) must not hold a canceled evaluation: the wait is polled, the
/// stalled worker is killed and reaped, and the call answers Canceled within a few hundred ms.
#[test]
fn canceled_evaluation_does_not_wait_for_a_stalled_hello() {
    bounded_case("canceled_evaluation_does_not_wait_for_a_stalled_hello", || {
        let mut options = worker_options(WorkerTestFault::StallHello, 4);
        options.worker.spawn_timeout = Duration::from_secs(30);
        let stalled = provider(QuotaSpec::default(), options);
        let token = CancellationToken::default();
        let canceller = {
            let token = token.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(300));
                token.cancel();
            })
        };
        let started = Instant::now();
        let (result, _) = run(&stalled, "1+2", 1, 1000, 0, &token);
        let waited = started.elapsed();
        canceller.join().unwrap();
        assert_eq!(code(result), ExprErrorCode::Canceled);
        assert!(waited < Duration::from_secs(5), "the HELLO wait ignored the cancel: {waited:?}");
        let workers = worker_stats(&stalled);
        assert_eq!(
            (workers.spawned, workers.killed_canceled, workers.spawn_failed, workers.crashed),
            (1, 1, 0, 0),
            "{workers:?}"
        );
        assert!(!stalled.is_poisoned());
        assert!(stalled.retire().is_clean());

        // Without a cancel the spawn timeout still bounds the wait (typed spawn fault, not a hang).
        let mut options = worker_options(WorkerTestFault::StallHello, 4);
        options.worker.spawn_timeout = Duration::from_millis(400);
        let timed_out = provider(QuotaSpec::default(), options);
        let started = Instant::now();
        assert_eq!(code(eval(&timed_out, "1+2", 1)), ExprErrorCode::Unsupported);
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        assert_eq!(timed_out.stats().faulted(EngineFault::SpawnFailed), 1);
        assert!(!timed_out.is_poisoned());
        assert!(timed_out.retire().is_clean());
    });
}
