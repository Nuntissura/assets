#![cfg(feature = "quickjs")]
//! Operating-system caps of the worker process (run with `--features quickjs`; the
//! `motion-js-worker` binary is built by cargo for this test). A cap is observed by the HOST through a
//! kernel notification, so the typed budget cannot be forged or swallowed by script text. Policy
//! expectations come from the lane task (memory and CPU caps, typed result, `Unavailable` elsewhere);
//! the derived caps are hand-computed from the default quotas (memory 16 MiB + stack 512 KiB + 128 MiB
//! slack; CPU 250 ms deadline + 250 ms kill grace + 500 ms slack = 1 s).
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

struct NoHost;

impl HostRead for NoHost {
    fn read(&self, _path: &PropertyPath, _tick: EvalTick, _fuel: &mut Fuel) -> Result<Vec<f64>, ExprError> {
        Err(ExprError::new(ExprErrorCode::ReferenceMissing))
    }
}

type P = MotionJsProvider<QuickJsBackend>;

/// Hostile source must reach the engine and the worker supervisor, so no profile validator here.
fn provider(spec: QuotaSpec, os: OsCapConfig) -> P {
    let quotas = Quotas::try_from_spec(spec).unwrap();
    let mut options = QuickJsOptions::default().with_worker_program(WORKER);
    options.worker.os = os;
    MotionJsProvider::new(QuickJsBackend::new(&quotas, options).unwrap(), quotas).without_validator()
}

fn eval_with(p: &P, source: &str, fuel: u64) -> Result<Vec<f64>, ExprError> {
    let request = ExprRequest { source, revision: 0, tick: EvalTick::new(0), seed: 0, dimension: 1, base: None };
    p.evaluate(&request, &NoHost, &mut Fuel::new(fuel), &CancellationToken::default())
}

fn eval(p: &P, source: &str) -> Result<Vec<f64>, ExprError> {
    eval_with(p, source, 1_000)
}

fn code(result: Result<Vec<f64>, ExprError>) -> ExprErrorCode {
    result.expect_err("expected a typed rejection").code
}

fn workers(p: &P) -> WorkerStats {
    p.engine().worker_stats().expect("worker backend")
}

/// 1 MiB strings until the process cannot commit more.
const BOMB: &str = "var a = []; for (;;) { a.push('x'.repeat(1048576)) }";
/// The same bomb, but the script catches the engine's out-of-memory error and returns normally.
const SWALLOWED_BOMB: &str = "var a = []; try { for (;;) { a.push('x'.repeat(1048576)) } } catch (e) { a = null } 5";

#[test]
fn os_cap_status_is_typed_per_platform() {
    bounded_case("os_cap_status_is_typed_per_platform", || {
        let quotas = Quotas::default();
        let backend = |policy| {
            let mut options = QuickJsOptions::default().with_worker_program(WORKER);
            options.worker.os.policy = policy;
            QuickJsBackend::new(&quotas, options)
        };
        if cfg!(windows) {
            let backend = backend(OsCapPolicy::Required).unwrap();
            assert_eq!(backend.os_caps(), OsCapStatus::Enforced);
            // Derived caps: (16 MiB + 512 KiB) + 128 MiB memory; 250 ms + 250 ms + 500 ms CPU.
            assert_eq!(
                backend.os_limits(),
                Some(OsLimits { process_memory_bytes: (16 << 20) + (512 << 10) + (128 << 20), cpu_time: Duration::from_secs(1) })
            );
        } else {
            let tolerant = backend(OsCapPolicy::WhereAvailable).unwrap();
            assert_eq!(tolerant.os_caps(), OsCapStatus::Unavailable(OsCapUnavailable::PlatformNotSupported));
            assert_eq!(tolerant.os_limits(), None);
            assert_eq!(backend(OsCapPolicy::Required).err(), Some(QuickJsConfigError::OsCapsUnavailable));
        }
        // An explicit cap that cannot hold the worker, or a zero CPU cap, is a configuration error.
        for limits in [
            OsLimits { process_memory_bytes: 1 << 20, cpu_time: Duration::from_secs(1) },
            OsLimits { process_memory_bytes: 256 << 20, cpu_time: Duration::ZERO },
        ] {
            let mut options = QuickJsOptions::default().with_worker_program(WORKER);
            options.worker.os.limits = Some(limits);
            let result = QuickJsBackend::new(&quotas, options);
            if cfg!(windows) {
                assert_eq!(result.err(), Some(QuickJsConfigError::WorkerPolicyInvalid));
            } else {
                assert!(result.is_ok(), "caps are not applied on this platform");
            }
        }
    });
}

#[cfg(windows)]
#[test]
fn job_memory_cap_gives_typed_budget_even_when_the_script_swallows_the_error() {
    bounded_case("job_memory_cap_gives_typed_budget_even_when_the_script_swallows_the_error", || {
        // Engine heap limit 512 MiB, operating-system cap 96 MiB: the cap fires first.
        let capped = provider(
            QuotaSpec { memory_bytes: 512 << 20, deadline: Duration::from_secs(10), ..QuotaSpec::default() },
            OsCapConfig {
                limits: Some(OsLimits { process_memory_bytes: 96 << 20, cpu_time: Duration::from_secs(60) }),
                ..OsCapConfig::default()
            },
        );
        assert_eq!(eval(&capped, "1+2"), Ok(vec![3.0]));
        let started = Instant::now();
        assert_eq!(code(eval(&capped, BOMB)), ExprErrorCode::Budget);
        assert!(started.elapsed() < Duration::from_secs(30), "{:?}", started.elapsed());
        let stats = capped.stats();
        assert_eq!((stats.denied(BudgetKind::Memory), stats.denied(BudgetKind::Fuel), stats.denied(BudgetKind::Deadline)), (1, 0, 0));
        let w = workers(&capped);
        assert_eq!((w.killed_os_memory, w.killed_os_cpu, w.crashed, w.killed_deadline), (1, 0, 0, 0));

        // The script catches the engine's own out-of-memory error and returns 5: the host still
        // reports the kernel-observed cap, five times in a row (no lost-notice race).
        for round in 2..=6_u64 {
            assert_eq!(code(eval(&capped, SWALLOWED_BOMB)), ExprErrorCode::Budget, "round {round}");
            assert_eq!(workers(&capped).killed_os_memory, round);
        }
        assert_eq!(capped.stats().denied(BudgetKind::Memory), 6);
        // The provider is healthy and the next evaluation runs on a fresh worker.
        let spawned = workers(&capped).spawned;
        assert_eq!(eval(&capped, "var t = 0; for (var i = 1; i <= 100; i++) { t += i } t"), Ok(vec![5050.0]));
        assert_eq!(workers(&capped).spawned, spawned + 1, "the killed worker was replaced, not reused");
        assert!(!capped.is_poisoned());
        assert!(capped.retire().is_clean());

        // Control: the same swallowing script under the derived cap and a small ENGINE heap limit is
        // stopped by the engine, which a script can catch: it returns 5 and no budget is counted.
        // Forged out-of-memory text is a thrown error, never a budget.
        let engine_limited = provider(
            QuotaSpec { memory_bytes: 8 << 20, ..QuotaSpec::default() },
            OsCapConfig::default(),
        );
        assert_eq!(eval(&engine_limited, SWALLOWED_BOMB), Ok(vec![5.0]));
        assert_eq!(code(eval(&engine_limited, "throw new InternalError('out of memory')")), ExprErrorCode::Unsupported);
        let stats = engine_limited.stats();
        assert_eq!((stats.denied(BudgetKind::Memory), workers(&engine_limited).killed_os_memory), (0, 0));
        assert!(engine_limited.retire().is_clean());
    });
}

#[cfg(windows)]
#[test]
fn job_cpu_cap_gives_typed_budget_and_is_rearmed_per_request() {
    bounded_case("job_cpu_cap_gives_typed_budget_and_is_rearmed_per_request", || {
        // Deadline at the 10 s ceiling and an effectively unlimited Fuel: only the 400 ms CPU cap can stop
        // `while (true) {}`.
        let p = provider(
            QuotaSpec { deadline: Duration::from_secs(10), ..QuotaSpec::default() },
            OsCapConfig {
                limits: Some(OsLimits { process_memory_bytes: 256 << 20, cpu_time: Duration::from_millis(400) }),
                ..OsCapConfig::default()
            },
        );
        // A worker's CPU time accumulates over its whole life, so a pooled worker must get a fresh
        // allowance per request: run short scripts (each far below the cap) until the summed wall time
        // is six times the cap. Not one of them may be stopped, and one worker serves them all.
        let work = "var s = 0; for (var i = 0; i < 100000; i++) { s += i } s";
        let mut spent = Duration::ZERO;
        let mut runs = 0_u32;
        while spent < Duration::from_millis(2_400) && runs < 5_000 {
            let started = Instant::now();
            assert_eq!(eval_with(&p, work, 1_000_000_000), Ok(vec![4_999_950_000.0]), "run {runs}");
            spent += started.elapsed();
            runs += 1;
        }
        eprintln!("cpu_rearm: {runs} requests in {spent:?} against a 400ms CPU cap");
        assert!(spent >= Duration::from_millis(2_400), "the loop must outlast six caps, ran {runs} requests in {spent:?}");
        assert_eq!(workers(&p).spawned, 1, "one pooled worker served {runs} requests");
        assert_eq!(workers(&p).killed_os_cpu, 0);

        let started = Instant::now();
        assert_eq!(code(eval_with(&p, "while (true) {}", 1_000_000_000_000)), ExprErrorCode::Budget);
        let elapsed = started.elapsed();
        eprintln!("cpu_cap: infinite loop stopped after {elapsed:?}");
        // The cap is CPU time, not wall time, and the test process cannot read the worker's CPU time, so the
        // wall clock is no product measure: under host load (R39 saw 4.54 s) the worker is simply descheduled.
        // The product proof is the typed Budget(Cpu) above with zero Deadline and Fuel denials and one
        // killed_os_cpu below. The wall clock only guards: at least the cap's 300 ms floor (an earlier stop
        // would not be the 400 ms allowance) and well under the 10 s deadline, so the deadline did not fire.
        assert!(elapsed >= Duration::from_millis(300) && elapsed < Duration::from_secs(9), "{elapsed:?}");
        let stats = p.stats();
        assert_eq!(
            (stats.denied(BudgetKind::Cpu), stats.denied(BudgetKind::Deadline), stats.denied(BudgetKind::Fuel)),
            (1, 0, 0)
        );
        let w = workers(&p);
        assert_eq!((w.killed_os_cpu, w.killed_deadline, w.killed_os_memory), (1, 0, 0));
        // The over-budget worker was dropped; the provider recovers on a new worker.
        assert_eq!(eval(&p, "1+2"), Ok(vec![3.0]));
        assert_eq!(workers(&p).spawned, 2);
        assert!(p.retire().is_clean());
    });
}
