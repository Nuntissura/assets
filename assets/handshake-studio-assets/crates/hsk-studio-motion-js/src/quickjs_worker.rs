//! Worker-process isolation for the QuickJS engine (EXPR-PROVIDER-04/08, slice-2 findings
//! `async-syntax-leak-abort` and `interrupt-is-cooperative`).
//!
//! Host side (`WorkerEngine`): a bounded pool of `motion-js-worker` child processes. One evaluation
//! attempt = one request frame on the worker's stdin and one reply frame on its stdout (see
//! `quickjs_wire`); blocking pipe I/O lives on a per-worker I/O thread, so the calling thread only
//! waits on a channel and can enforce, in order of precedence:
//! * cancel: the worker is killed and the evaluation returns `Canceled`;
//! * wall-clock deadline (injected-clock remainder + `kill_grace`, measured on the real monotonic
//!   clock): the worker is killed and the evaluation returns `Budget(Deadline)`;
//! * output cap: a reply frame above the cap kills the worker and returns `Output`;
//! * crash: end of stream (abort, native crash, external kill) returns `Faulted(Crashed)` and the
//!   worker is reaped. The provider stays usable; the next call starts a fresh worker, but at most
//!   `max_spawns` workers may start per `spawn_window` (`Faulted(RespawnLimited)` beyond that).
//!
//! Only processes this engine spawned are ever killed (it holds the `Child` handles). Workers get an
//! empty environment (no inherited secrets), a null stderr, piped stdin/stdout and, on Windows,
//! `CREATE_NO_WINDOW`, so they never show a console window or take foreground focus.
//!
//! Worker side (`worker_main`): speaks HELLO, then serves requests until stdin closes. Each request
//! builds a fresh Runtime/context (`run_realm`) on a sized thread; the worker also self-terminates
//! if that thread outlives the request deadline by `WATCHDOG_GRACE` (parent gone or kill missed).
//! The heap limit is applied inside the worker with readback before any context exists (EXPR-PROVIDER-02).
//!
//! Operating-system caps (`os_caps`, SMB-MOTION-JS-SANDBOX-2): on Windows each worker also runs in a
//! Job Object with a process memory cap, a per-request CPU time cap, kill-on-close, die-on-unhandled-
//! exception and a one-process limit. The host drains the job's completion port after every request;
//! a memory-cap or CPU-cap notice from the kernel makes the attempt `Budget(Memory)` / `Budget(Cpu)`
//! even if the script swallowed the engine's own error, and the worker is killed. These classes come
//! from a kernel event, never from script text. Other platforms report `OsCapStatus::Unavailable`.
//!
//! Process handling (spawn with an empty environment and no window, deadline/kill/reap, crash class,
//! Job Object caps, program lookup beside the executable) lives in `hsk-studio-worker`; this file gives
//! it the QuickJS protocol, the pool and the typed engine failures.
#![deny(clippy::arithmetic_side_effects)]
use crate::{
    clock::SystemClock,
    engine::{
        BudgetKind, EngineFailure, EngineFault, EngineIdentity, EngineRun, EvalJob, HostBridge,
        JsValue, RealmReceipt,
    },
    limits::{OUTPUT_NODES_CEILING, Quotas},
    os_caps::{OsCapConfig, OsCapPolicy, OsCapStatus, platform_status},
    quickjs::{
        ENGINE_VERSION, Input, LimitReadback, QuickJsConfigError, QuickJsEngine, RunOutcome,
        RunReport, THREAD_STACK_HEADROOM, drive, internal_report, run_realm, validate_limits,
    },
    quickjs_wire as wire,
};
#[cfg(feature = "test-faults")]
use crate::quickjs::WorkerTestFault;
use hsk_studio_accord::CancellationToken;
use hsk_studio_worker::{
    EXIT_PATIENCE, OsLimits, ReadFailure, RespawnGate, Session, StartError, StartRequest,
    Supervisor, SupervisorConfig, Waited, after, harden_worker_process, resolve_program,
};
use std::{
    io,
    path::PathBuf,
    process::ChildStdout,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

/// File name of the worker executable (without the platform suffix).
pub const WORKER_PROGRAM: &str = "motion-js-worker";
/// The worker kills itself this long after its request deadline if nobody stopped it first.
const WATCHDOG_GRACE: Duration = Duration::from_secs(2);

/// Worker-process policy.
#[derive(Clone, Debug)]
pub struct WorkerConfig {
    /// Worker executable. `None`: `motion-js-worker` in the directory of the running executable
    /// (that directory only; a layout with the worker elsewhere passes the path explicitly, see
    /// [`WorkerConfig::with_worker_program`]).
    pub program: Option<PathBuf>,
    /// Bounded respawn rate: at most this many workers start per `spawn_window`.
    pub max_spawns: u32,
    pub spawn_window: Duration,
    /// Time a new worker gets to complete its HELLO handshake.
    pub spawn_timeout: Duration,
    /// Added to the evaluation deadline before the host kills the worker, so a cooperative
    /// in-engine deadline normally answers first.
    pub kill_grace: Duration,
    /// Operating-system caps for each worker process.
    pub os: OsCapConfig,
    /// Fault the spawned workers inject (`test-faults` feature only; product builds have no field).
    #[cfg(feature = "test-faults")]
    pub test_fault: WorkerTestFault,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            program: None,
            max_spawns: 16,
            spawn_window: Duration::from_secs(10),
            spawn_timeout: Duration::from_secs(10),
            kill_grace: Duration::from_millis(250),
            os: OsCapConfig::default(),
            #[cfg(feature = "test-faults")]
            test_fault: WorkerTestFault::None,
        }
    }
}

impl WorkerConfig {
    /// Points at a worker executable outside the host executable's directory (D14).
    pub fn with_worker_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = Some(program.into());
        self
    }
}

/// Monotonic process counters of one worker pool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkerStats {
    pub spawned: u64,
    pub spawn_failed: u64,
    /// Spawns refused by the bounded respawn rate.
    pub spawn_denied: u64,
    pub crashed: u64,
    pub protocol_faults: u64,
    pub killed_deadline: u64,
    pub killed_canceled: u64,
    pub killed_oversize: u64,
    /// Workers killed because the operating system reported the memory cap was hit.
    pub killed_os_memory: u64,
    /// Workers killed because the operating system reported the CPU cap was hit.
    pub killed_os_cpu: u64,
    /// Exit code of the most recent crashed worker (platform value, e.g. an NTSTATUS on Windows).
    pub last_crash_exit_code: Option<i32>,
}

#[derive(Default)]
struct Counters {
    spawned: AtomicU64,
    spawn_failed: AtomicU64,
    spawn_denied: AtomicU64,
    crashed: AtomicU64,
    protocol_faults: AtomicU64,
    killed_deadline: AtomicU64,
    killed_canceled: AtomicU64,
    killed_oversize: AtomicU64,
    killed_os_memory: AtomicU64,
    killed_os_cpu: AtomicU64,
    last_crash_exit_code: Mutex<Option<i32>>,
}

fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::AcqRel);
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

type WorkerSession = Session<Vec<u8>, wire::WireError>;

/// The supervisor's frame reader for this protocol.
fn read_reply(
    stdout: &mut ChildStdout,
    cap: u32,
) -> Result<Vec<u8>, ReadFailure<wire::WireError>> {
    let cap = usize::try_from(cap).unwrap_or(usize::MAX);
    wire::read_frame(stdout, cap).map_err(|error| match error {
        wire::WireError::Closed => ReadFailure::Closed,
        other => ReadFailure::Failed(other),
    })
}

/// A request payload with its length prefix, as the supervisor writes it; `None` above the frame limit.
fn framed(payload: &[u8]) -> Option<Vec<u8>> {
    let mut frame = Vec::with_capacity(payload.len().saturating_add(4));
    wire::write_frame(&mut frame, payload).ok()?;
    Some(frame)
}

fn frame_cap(cap: usize) -> u32 {
    u32::try_from(cap).unwrap_or(u32::MAX)
}

/// Process-isolated QuickJS engine (host side).
pub(crate) struct WorkerEngine {
    memory_bytes: usize,
    stack_bytes: usize,
    supervisor: Supervisor,
    respawn: RespawnGate,
    spawn_timeout: Duration,
    kill_grace: Duration,
    os_status: OsCapStatus,
    os_limits: Option<OsLimits>,
    max_idle: usize,
    idle: Mutex<Vec<WorkerSession>>,
    request_ids: AtomicU64,
    counters: Counters,
}

impl WorkerEngine {
    pub(crate) fn new(quotas: &Quotas, config: &WorkerConfig) -> Result<Self, QuickJsConfigError> {
        let (memory_bytes, stack_bytes) =
            validate_limits(quotas.memory_bytes(), quotas.stack_bytes())?;
        if config.max_spawns == 0
            || config.spawn_window.is_zero()
            || config.spawn_timeout.is_zero()
            || config.kill_grace.is_zero()
        {
            return Err(QuickJsConfigError::WorkerPolicyInvalid);
        }
        let program = resolve_program(config.program.clone(), WORKER_PROGRAM)
            .ok_or(QuickJsConfigError::WorkerProgramMissing)?;
        let os_status = platform_status();
        let os_limits = match (os_status, config.os.policy) {
            (OsCapStatus::Enforced, _) => Some(config.os.resolve(quotas, config.kill_grace)?),
            (OsCapStatus::Unavailable(_), OsCapPolicy::Required) => {
                return Err(QuickJsConfigError::OsCapsUnavailable);
            }
            (OsCapStatus::Unavailable(_), OsCapPolicy::WhereAvailable) => None,
        };
        #[cfg(feature = "test-faults")]
        let env: Vec<(String, String)> = config
            .test_fault
            .env_value()
            .map(|value| (WorkerTestFault::ENV.to_owned(), value.to_owned()))
            .into_iter()
            .collect();
        #[cfg(not(feature = "test-faults"))]
        let env: Vec<(String, String)> = Vec::new();
        Ok(Self {
            memory_bytes,
            stack_bytes,
            supervisor: Supervisor::new(SupervisorConfig {
                program,
                args: Vec::new(),
                env,
                max_workers: usize::MAX,
                max_in_flight_bytes: u64::MAX,
                spawn_timeout: config.spawn_timeout,
                os_limits,
                reply_buffer: None,
                io_thread_name: "motion-js-worker-io".to_owned(),
            }),
            respawn: RespawnGate::new(
                usize::try_from(config.max_spawns).unwrap_or(usize::MAX),
                config.spawn_window,
            ),
            spawn_timeout: config.spawn_timeout,
            kill_grace: config.kill_grace,
            os_status,
            os_limits,
            max_idle: usize::try_from(quotas.max_concurrent_realms())
                .unwrap_or(1)
                .max(1),
            idle: Mutex::new(Vec::new()),
            request_ids: AtomicU64::new(1),
            counters: Counters::default(),
        })
    }

    pub(crate) fn stats(&self) -> WorkerStats {
        let c = &self.counters;
        WorkerStats {
            spawned: c.spawned.load(Ordering::Acquire),
            spawn_failed: c.spawn_failed.load(Ordering::Acquire),
            spawn_denied: c.spawn_denied.load(Ordering::Acquire),
            crashed: c.crashed.load(Ordering::Acquire),
            protocol_faults: c.protocol_faults.load(Ordering::Acquire),
            killed_deadline: c.killed_deadline.load(Ordering::Acquire),
            killed_canceled: c.killed_canceled.load(Ordering::Acquire),
            killed_oversize: c.killed_oversize.load(Ordering::Acquire),
            killed_os_memory: c.killed_os_memory.load(Ordering::Acquire),
            killed_os_cpu: c.killed_os_cpu.load(Ordering::Acquire),
            last_crash_exit_code: *lock(&c.last_crash_exit_code),
        }
    }

    /// Whether worker processes run under operating-system caps.
    pub(crate) fn os_status(&self) -> OsCapStatus {
        self.os_status
    }

    /// The caps applied to each worker process (`None` where the platform has none).
    pub(crate) fn os_limits(&self) -> Option<OsLimits> {
        self.os_limits
    }

    fn spawn(&self, cancel: &CancellationToken) -> Result<WorkerSession, EngineFailure> {
        if !self.respawn.admit() {
            bump(&self.counters.spawn_denied);
            return Err(EngineFailure::Faulted(EngineFault::RespawnLimited));
        }
        let spawn_failed = || {
            bump(&self.counters.spawn_failed);
            EngineFailure::Faulted(EngineFault::SpawnFailed)
        };
        let permit = self.supervisor.admit(0).map_err(|_| spawn_failed())?;
        let started = self.supervisor.start(
            permit,
            StartRequest {
                owner_session: "motion-js",
                extra_args: &[],
                keepalive: None,
                hello_cap: frame_cap(wire::HELLO_CAP),
            },
            read_reply,
            None,
        );
        let session = match started {
            Ok(session) => session,
            Err(StartError::NotStarted) => return Err(spawn_failed()),
            Err(StartError::Setup) => {
                bump(&self.counters.spawned);
                return Err(spawn_failed());
            }
        };
        bump(&self.counters.spawned);
        // The HELLO wait is polled in short slices so the caller's cancel is honoured within one
        // slice (a stalled or slow-starting worker must not hold a canceled evaluation for the whole
        // `spawn_timeout`); `spawn_timeout` still bounds the wait. A canceled spawn drops the session,
        // which kills and reaps the child. Test `canceled_evaluation_does_not_wait_for_a_stalled_hello`.
        match session.wait(cancel, after(self.spawn_timeout)) {
            Waited::Frame(hello) if wire::decode_hello(&hello).is_ok() => Ok(session),
            Waited::Frame(_) => {
                bump(&self.counters.protocol_faults);
                Err(EngineFailure::Faulted(EngineFault::Protocol))
            }
            Waited::Canceled => {
                bump(&self.counters.killed_canceled);
                Err(EngineFailure::Canceled)
            }
            Waited::CpuCap | Waited::TimedOut | Waited::Failed(_) => Err(spawn_failed()),
        }
    }

    fn checkout(&self, cancel: &CancellationToken) -> Result<WorkerSession, EngineFailure> {
        loop {
            let Some(mut session) = lock(&self.idle).pop() else {
                return self.spawn(cancel);
            };
            if session.is_alive() {
                return Ok(session);
            }
            session.terminate(Duration::ZERO);
        }
    }

    fn checkin(&self, session: WorkerSession) {
        let mut idle = lock(&self.idle);
        if idle.len() < self.max_idle {
            idle.push(session);
        }
    }

    /// Sends one request frame and waits for the reply. Every failure terminates the worker and
    /// reports whether the process is provably gone.
    fn exchange(
        &self,
        session: &mut WorkerSession,
        frame: &[u8],
        cap: usize,
        cancel: &CancellationToken,
        wall: Instant,
    ) -> Result<Vec<u8>, (EngineFailure, bool)> {
        let sent = framed(frame).is_some_and(|bytes| session.send_request(bytes, frame_cap(cap)));
        let waited = if sent {
            session.wait(cancel, wall)
        } else {
            Waited::Failed(ReadFailure::Closed)
        };
        let mut events = session.drain_os_events();
        let (mut failure, patience) = match waited {
            Waited::Frame(bytes) if events.is_clear() => return Ok(bytes),
            // A reply arrived but the kernel reports a cap was hit: the cause below replaces it.
            Waited::Frame(_) => (EngineFailure::Internal, Duration::ZERO),
            Waited::Canceled => (EngineFailure::Canceled, Duration::ZERO),
            Waited::CpuCap => (EngineFailure::Budget(BudgetKind::Cpu), Duration::ZERO),
            Waited::TimedOut => (EngineFailure::Budget(BudgetKind::Deadline), Duration::ZERO),
            Waited::Failed(ReadFailure::Failed(wire::WireError::TooLarge)) => {
                (EngineFailure::Output, Duration::ZERO)
            }
            Waited::Failed(ReadFailure::Failed(wire::WireError::Malformed)) => (
                EngineFailure::Faulted(EngineFault::Protocol),
                Duration::ZERO,
            ),
            Waited::Failed(ReadFailure::Closed | ReadFailure::Failed(wire::WireError::Closed)) => {
                (EngineFailure::Faulted(EngineFault::Crashed), EXIT_PATIENCE)
            }
        };
        let done = session.terminate(patience);
        if failure == EngineFailure::Faulted(EngineFault::Crashed) {
            // The kernel may post the cap notice just before the process disappears.
            events.merge(session.drain_os_events());
        }
        // The caller's own cancel wins; otherwise a kernel-reported cap is the root cause.
        if failure != EngineFailure::Canceled {
            if events.memory {
                failure = EngineFailure::Budget(BudgetKind::Memory);
            } else if events.cpu {
                failure = EngineFailure::Budget(BudgetKind::Cpu);
            }
        }
        match failure {
            EngineFailure::Canceled => bump(&self.counters.killed_canceled),
            EngineFailure::Budget(BudgetKind::Deadline) => bump(&self.counters.killed_deadline),
            EngineFailure::Budget(BudgetKind::Memory) => bump(&self.counters.killed_os_memory),
            EngineFailure::Budget(BudgetKind::Cpu) => bump(&self.counters.killed_os_cpu),
            EngineFailure::Output => bump(&self.counters.killed_oversize),
            EngineFailure::Faulted(EngineFault::Protocol) => bump(&self.counters.protocol_faults),
            EngineFailure::Faulted(EngineFault::Crashed) => {
                bump(&self.counters.crashed);
                *lock(&self.counters.last_crash_exit_code) = done.exit_code;
            }
            _ => {}
        }
        Err((failure, done.reaped))
    }

    fn run_once(&self, input: Input<'_>) -> RunOutcome {
        let failed = |failure, dropped| RunOutcome::Failed { failure, dropped };
        let before_ns = input.clock.now_ns();
        let mut session = match self.checkout(&input.cancel) {
            Ok(session) => session,
            // No process exists, so there is nothing left to retire.
            Err(failure) => return failed(failure, true),
        };
        // Starting a process is host setup, not script time: the script keeps the whole budget that
        // was left before the checkout and the replay loop grants the setup time back to the host
        // deadline.
        let setup_ns = input.clock.now_ns().saturating_sub(before_ns);
        let remaining_ns = input.deadline_ns.saturating_sub(before_ns);
        if let Some(limits) = self.os_limits
            && session.has_os_caps()
            && session.arm_cpu(limits.cpu_time).is_err()
        {
            // Without a CPU allowance the worker would run uncapped: fail the attempt, drop the worker.
            bump(&self.counters.spawn_failed);
            let done = session.terminate(Duration::ZERO);
            return failed(EngineFailure::Faulted(EngineFault::SpawnFailed), done.reaped);
        }
        let id = self.request_ids.fetch_add(1, Ordering::AcqRel);
        let frame = wire::encode_eval(id, &input, remaining_ns);
        if frame.len() > wire::MAX_REQUEST_FRAME {
            self.checkin(session);
            return failed(EngineFailure::Budget(BudgetKind::Memory), true);
        }
        let wall = after(Duration::from_nanos(remaining_ns).saturating_add(self.kill_grace));
        let reply = self.exchange(
            &mut session,
            &frame,
            wire::response_cap(&input),
            &input.cancel,
            wall,
        );
        match reply {
            Ok(bytes) => match wire::decode_report(&bytes, id, input.max_nodes) {
                Ok(report) => {
                    self.checkin(session);
                    RunOutcome::Report { report, setup_ns }
                }
                Err(_) => {
                    bump(&self.counters.protocol_faults);
                    let done = session.terminate(Duration::ZERO);
                    failed(EngineFailure::Faulted(EngineFault::Protocol), done.reaped)
                }
            },
            Err((failure, gone)) => failed(failure, gone),
        }
    }

    /// Applies the configured limits to a throw-away runtime inside a worker and reads them back.
    pub(crate) fn verify_limits(&self) -> Result<LimitReadback, EngineFailure> {
        let mut session = self.checkout(&CancellationToken::default())?;
        let id = self.request_ids.fetch_add(1, Ordering::AcqRel);
        let frame = wire::encode_verify(
            id,
            u64::try_from(self.memory_bytes).unwrap_or(u64::MAX),
            u64::try_from(self.stack_bytes).unwrap_or(u64::MAX),
        );
        let wall = after(self.spawn_timeout);
        let bytes = self
            .exchange(
                &mut session,
                &frame,
                wire::HELLO_CAP,
                &CancellationToken::default(),
                wall,
            )
            .map_err(|(failure, _)| failure)?;
        match wire::decode_limits(&bytes, id) {
            Ok(Some(readback)) => {
                self.checkin(session);
                Ok(readback)
            }
            Ok(None) => {
                self.checkin(session);
                Err(EngineFailure::Internal)
            }
            Err(_) => {
                bump(&self.counters.protocol_faults);
                session.terminate(Duration::ZERO);
                Err(EngineFailure::Faulted(EngineFault::Protocol))
            }
        }
    }

    pub(crate) fn identity(&self) -> EngineIdentity {
        EngineIdentity {
            name: "quickjs-ng via rquickjs (worker process)",
            version: ENGINE_VERSION,
            native: true,
        }
    }

    pub(crate) fn run(&self, job: &EvalJob<'_>, host: &mut dyn HostBridge) -> EngineRun {
        let mut realm = RealmReceipt {
            fresh: true,
            dropped: true,
        };
        let outcome: Result<JsValue, EngineFailure> = drive(
            self.memory_bytes,
            self.stack_bytes,
            job,
            host,
            &mut realm,
            |input| self.run_once(input),
        );
        EngineRun { outcome, realm }
    }
}

// ---------------------------------------------------------------------------------------------
// Worker process side.
// ---------------------------------------------------------------------------------------------

/// Fault this worker injects, read once from its environment (`test-faults` feature only: a product
/// worker has no such read, `tests/worker_fault_env.rs`).
#[cfg(feature = "test-faults")]
fn injected_fault() -> WorkerTestFault {
    static FAULT: std::sync::OnceLock<WorkerTestFault> = std::sync::OnceLock::new();
    *FAULT.get_or_init(WorkerTestFault::from_env)
}

fn evaluate(request: wire::EvalRequest) -> Option<RunReport> {
    let Ok((memory_bytes, stack_bytes)) =
        validate_limits(request.memory_bytes, request.stack_bytes)
    else {
        return Some(internal_report());
    };
    #[cfg(feature = "test-faults")]
    {
        if injected_fault() == WorkerTestFault::ExitMidRequest
            && request.source.contains("exit_now")
        {
            std::process::exit(86);
        }
    }
    let remaining = Duration::from_nanos(request.remaining_ns);
    let (done, result) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("motion-js-quickjs".to_owned())
        .stack_size(stack_bytes.saturating_add(THREAD_STACK_HEADROOM))
        .spawn(move || {
            let input = Input {
                source: &request.source,
                max_nodes: usize::try_from(request.max_nodes)
                    .unwrap_or(OUTPUT_NODES_CEILING)
                    .min(OUTPUT_NODES_CEILING),
                memory_bytes,
                stack_bytes,
                allowance: request.allowance,
                // The host enforces cancel by killing this process; the realm never sees a token.
                cancel: CancellationToken::default(),
                clock: Arc::new(SystemClock::new()),
                deadline_ns: request.remaining_ns,
                cache: &request.cache,
                utc: request.utc,
                #[cfg(feature = "test-faults")]
                fault: injected_fault(),
            };
            let _ = done.send(run_realm(input));
        });
    spawned.ok()?;
    match result.recv_timeout(remaining.saturating_add(WATCHDOG_GRACE)) {
        Ok(report) => Some(report),
        // The realm thread outlived its deadline (non-cooperative script, or the host is gone).
        Err(RecvTimeoutError::Timeout) => std::process::exit(3),
        Err(RecvTimeoutError::Disconnected) => None,
    }
}

fn verify(memory: u64, stack: u64) -> Result<LimitReadback, ()> {
    QuickJsEngine::from_raw(memory, stack)
        .map_err(|_| ())?
        .verify_limits()
        .map_err(|_| ())
}

/// Entry point of the `motion-js-worker` binary; returns the process exit code. Reads request
/// frames from stdin, writes reply frames to stdout, and exits 0 when stdin closes.
pub fn worker_main() -> i32 {
    harden_worker_process();
    #[cfg(feature = "test-faults")]
    {
        if injected_fault() == WorkerTestFault::StallHello {
            thread::sleep(Duration::from_secs(60));
        }
    }
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if wire::write_frame(&mut output, &wire::encode_hello()).is_err() {
        return 1;
    }
    loop {
        let frame = match wire::read_frame(&mut input, wire::MAX_REQUEST_FRAME) {
            Ok(frame) => frame,
            Err(wire::WireError::Closed) => return 0,
            Err(_) => return 2,
        };
        let reply = match wire::decode_request(&frame) {
            Ok(wire::Request::Eval(id, request)) => match evaluate(request) {
                Some(report) => wire::encode_report(id, &report),
                None => return 4,
            },
            Ok(wire::Request::Verify(id, memory, stack)) => {
                wire::encode_limits(id, verify(memory, stack))
            }
            Err(_) => return 2,
        };
        if wire::write_frame(&mut output, &reply).is_err() {
            return 1;
        }
    }
}

