//! Process supervision with no consumer vocabulary (moved here from score-plugin `supervisor.rs`,
//! motion-js `quickjs_worker.rs` and reel-native `host.rs`, Operator decision D18 option (a)).
//!
//! What it owns:
//! * spawn: `Command` with an empty environment (`env_clear`), a null stderr, piped stdin/stdout and, on
//!   Windows, `CREATE_NO_WINDOW`, so a worker is started without a console window (the absence of a
//!   window is NOT_PROVEN by any test); optional Windows Job Object caps (feature `job-caps`);
//! * admission: a worker-count and a byte budget acquired without ever waiting ([`Supervisor::admit`]),
//!   held by a [`Permit`] that is released only after the child has been reaped; [`RespawnGate`], a
//!   bounded respawn rate;
//! * ledger: every spawned process (owner session, start time, pid, killed/reaped/exit code) and
//!   monotonic counters ([`SupervisorStats`]);
//! * framed exchange: one request frame written, one or more reply frames read, by a per-worker I/O
//!   thread; the calling thread only waits on a channel in [`WAIT_SLICE`] slices, polling the cancel
//!   token, the wall-clock deadline and (with a Job Object) the kernel CPU accounting;
//! * classification and reclaim: cancel and deadline kill the child at once; end of stream is a crash
//!   (after [`EXIT_PATIENCE`] so the true exit code can be read); a framing failure supplied by the
//!   caller's reader is a protocol fault and kills the child; every path ends with the child killed or
//!   exited AND reaped; dropping a [`Session`] reaps it;
//! * liveness: [`Session::heartbeat`] probes the process without blocking.
//!
//! What it does not own: the wire format (the caller supplies a [`ReadFn`] that reads one reply frame and
//! maps its own framing errors into [`ReadFailure`], and pre-frames the request bytes), the meaning of
//! requests and replies, and error vocabulary (the caller maps [`Classified`] to its own errors). There
//! is no wire-level heartbeat: liveness of a request is its deadline. Only `Child` handles this module
//! spawned are ever killed.
#![deny(clippy::arithmetic_side_effects)]
use crate::{
    caps::{OsEvents, OsLimits, WorkerJob},
    time::after,
};
use hsk_studio_accord::CancellationToken;
use std::{
    collections::VecDeque,
    io::Write,
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// How often a waiting thread re-checks cancel and the wall-clock deadline.
pub const WAIT_SLICE: Duration = Duration::from_millis(5);
/// How long a worker that closed its stdout gets to exit by itself (to read its true exit code).
pub const EXIT_PATIENCE: Duration = Duration::from_millis(500);

/// Win32 `CREATE_NO_WINDOW` process creation flag (0x08000000, Process Creation Flags, Win32 docs).
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
fn quiet(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn quiet(_command: &mut Command) {}

/// One spawned process in the host-owned ledger (STU-PAR-005 shape; the real ledger is host-side).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessLedgerEntry {
    pub owner_session: String,
    pub started_unix_ms: u64,
    pub pid: u32,
    pub killed: bool,
    pub reaped: bool,
    pub exit_code: Option<i32>,
}

/// Monotonic process and admission counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SupervisorStats {
    pub spawned: u64,
    pub killed: u64,
    pub reaped: u64,
    pub crashed: u64,
    pub timed_out: u64,
    pub canceled: u64,
    pub protocol_faults: u64,
    /// Workers alive right now; every other spawned worker was reaped.
    pub workers_live: u64,
    /// Bytes currently reserved by live workers.
    pub in_flight_bytes: u64,
}

#[derive(Default)]
struct Counters {
    spawned: AtomicU64,
    killed: AtomicU64,
    reaped: AtomicU64,
    crashed: AtomicU64,
    timed_out: AtomicU64,
    canceled: AtomicU64,
    protocol_faults: AtomicU64,
}

fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::AcqRel);
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Bounded respawn rate: at most `max_spawns` admissions per `window`.
pub struct RespawnGate {
    max_spawns: usize,
    window: Duration,
    log: Mutex<VecDeque<Instant>>,
}

impl RespawnGate {
    pub fn new(max_spawns: usize, window: Duration) -> Self {
        Self {
            max_spawns,
            window,
            log: Mutex::new(VecDeque::new()),
        }
    }

    /// Records one start and returns true, or returns false (nothing recorded) when the rate is spent.
    pub fn admit(&self) -> bool {
        let now = Instant::now();
        let mut log = lock(&self.log);
        while log
            .front()
            .is_some_and(|started| now.duration_since(*started) > self.window)
        {
            log.pop_front();
        }
        if log.len() >= self.max_spawns {
            return false;
        }
        log.push_back(now);
        true
    }
}

/// Supervisor construction parameters (already validated by the caller).
#[derive(Clone, Debug)]
pub struct SupervisorConfig {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Environment variables set after `env_clear` (the child inherits nothing else).
    pub env: Vec<(String, String)>,
    pub max_workers: usize,
    pub max_in_flight_bytes: u64,
    /// Time a new worker gets to send its first (HELLO) frame ([`Supervisor::spawn`] only).
    pub spawn_timeout: Duration,
    /// Operating-system caps for each worker (needs feature `job-caps` on Windows; a worker whose caps
    /// cannot be applied is not started).
    pub os_limits: Option<OsLimits>,
    /// `Some(n)`: the reply channel holds at most `n` frames, so the I/O thread reads no further ahead
    /// (back-pressure). `None`: unbounded.
    pub reply_buffer: Option<usize>,
    /// Name of the per-worker I/O thread.
    pub io_thread_name: String,
}

struct Shared {
    config: SupervisorConfig,
    workers: AtomicUsize,
    bytes: AtomicU64,
    ledger: Mutex<Vec<ProcessLedgerEntry>>,
    counters: Counters,
}

/// The worker or byte budget has no room; nothing was acquired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused;

/// A held share of the worker and byte budget; released on drop, which for a session is after the reap.
pub struct Permit {
    shared: Arc<Shared>,
    bytes: u64,
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.shared.workers.fetch_sub(1, Ordering::AcqRel);
        self.shared.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

/// Why reading one reply frame failed. `E` is the caller's framing-error type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadFailure<E> {
    /// End of stream at a frame boundary, or an I/O failure: the peer is gone.
    Closed,
    /// The peer sent something that is not a valid frame.
    Failed(E),
}

/// Reads one reply frame of at most `cap` payload bytes from the worker's stdout.
pub type ReadFn<F, E> = fn(&mut ChildStdout, u32) -> Result<F, ReadFailure<E>>;

/// True for the reply frame that ends a request (the I/O thread stops reading after it). Without one,
/// every request has exactly one reply frame.
pub type TerminalFn<F> = fn(&F) -> bool;

/// Why a call ended without a usable reply frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure<E> {
    Canceled,
    Deadline,
    Read(ReadFailure<E>),
}

/// What [`Session::fail`] decided, for the caller to map into its own error vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Classified<E> {
    Canceled,
    Deadline,
    /// End of stream: the process exited (or was killed externally). `exit_code` when it exited itself.
    Crashed {
        exit_code: Option<i32>,
    },
    /// The caller's reader reported a framing failure; the worker was killed.
    Protocol(E),
}

/// What happened to the child when it was reclaimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terminated {
    pub killed: bool,
    pub reaped: bool,
    pub exit_code: Option<i32>,
}

/// Result of a non-blocking liveness probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    Running,
    /// The process has ended; `exit_code` is the platform value when it exited by itself.
    Exited { exit_code: Option<i32> },
    /// The operating system could not say.
    Unknown,
}

/// Why a spawn did not yield a session ([`Supervisor::spawn`], which also performs the HELLO wait).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnError<E> {
    /// The cancel token was set before anything was started.
    Canceled,
    /// The program could not be started, or its pipes or I/O thread could not be set up.
    Unavailable,
    /// The worker started but did not deliver a first frame; `Classified` says how it ended.
    Handshake(Classified<E>, Terminated),
}

/// Why [`Supervisor::start`] did not yield a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartError {
    /// The program could not be started: no process exists.
    NotStarted,
    /// A process was started but its caps, pipes or I/O thread could not be set up: it is already
    /// killed and reaped and counts as spawned.
    Setup,
}

struct Job {
    frame: Vec<u8>,
    cap: u32,
}

/// What [`Session::wait`] saw.
#[derive(Debug)]
pub enum Waited<F, E> {
    Frame(F),
    Canceled,
    /// The worker used more CPU than its allowance (kernel job accounting; needs `job-caps`).
    CpuCap,
    TimedOut,
    Failed(ReadFailure<E>),
}

enum Out<T> {
    Unbounded(mpsc::Sender<T>),
    Bounded(mpsc::SyncSender<T>),
}

impl<T> Out<T> {
    fn send(&self, value: T) -> Result<(), ()> {
        match self {
            Self::Unbounded(tx) => tx.send(value).map_err(|_| ()),
            Self::Bounded(tx) => tx.send(value).map_err(|_| ()),
        }
    }
}

/// One live worker process plus the I/O thread that owns its pipes.
pub struct Session<F, E> {
    child: Child,
    jobs: mpsc::Sender<Job>,
    replies: mpsc::Receiver<Result<F, ReadFailure<E>>>,
    /// False once the process was terminated or its stream is out of sync: never reused.
    usable: bool,
    ledger_index: usize,
    pid: u32,
    shared: Arc<Shared>,
    finished: Option<Terminated>,
    /// Operating-system caps of this worker (`None` where none were requested).
    job: Option<WorkerJob>,
    /// Caller state that must outlive the child (for example a private directory the child uses);
    /// dropped after `Drop::drop` has killed and reaped it, before the permit is released.
    _keepalive: Option<Box<dyn Send>>,
    /// Last field: released only after `Drop::drop` has killed and reaped the child.
    _permit: Permit,
}

/// Owns the worker's pipes: the first frame, then per job one write and the reply frames up to the
/// terminal one.
fn io_loop<F, E>(
    mut stdin: ChildStdin,
    mut stdout: ChildStdout,
    read: ReadFn<F, E>,
    terminal: Option<TerminalFn<F>>,
    hello_cap: u32,
    jobs: &mpsc::Receiver<Job>,
    out: &Out<Result<F, ReadFailure<E>>>,
) {
    let hello = read(&mut stdout, hello_cap);
    let failed = hello.is_err();
    if out.send(hello).is_err() || failed {
        return;
    }
    while let Ok(job) = jobs.recv() {
        if stdin
            .write_all(&job.frame)
            .and_then(|()| stdin.flush())
            .is_err()
        {
            let _ = out.send(Err(ReadFailure::Closed));
            return;
        }
        loop {
            let reply = read(&mut stdout, job.cap);
            let failed = reply.is_err();
            let ends = match (&reply, terminal) {
                (Ok(frame), Some(is_terminal)) => is_terminal(frame),
                _ => true,
            };
            if out.send(reply).is_err() || failed {
                return;
            }
            if ends {
                break;
            }
        }
    }
}

/// What one start needs beyond the supervisor's fixed configuration.
pub struct StartRequest<'a> {
    /// Owner recorded in the process ledger.
    pub owner_session: &'a str,
    /// Arguments appended to the configured ones for this start only.
    pub extra_args: &'a [String],
    /// Dropped once the child is reaped (or at once when no child was started).
    pub keepalive: Option<Box<dyn Send>>,
    /// Largest first (HELLO) frame the reader may accept.
    pub hello_cap: u32,
}

/// Process supervisor. Cheap to clone: clones share one budget, ledger and counters.
#[derive(Clone)]
pub struct Supervisor {
    shared: Arc<Shared>,
}

impl Supervisor {
    pub fn new(config: SupervisorConfig) -> Self {
        Self {
            shared: Arc::new(Shared {
                config,
                workers: AtomicUsize::new(0),
                bytes: AtomicU64::new(0),
                ledger: Mutex::new(Vec::new()),
                counters: Counters::default(),
            }),
        }
    }

    pub fn stats(&self) -> SupervisorStats {
        let c = &self.shared.counters;
        SupervisorStats {
            spawned: c.spawned.load(Ordering::Acquire),
            killed: c.killed.load(Ordering::Acquire),
            reaped: c.reaped.load(Ordering::Acquire),
            crashed: c.crashed.load(Ordering::Acquire),
            timed_out: c.timed_out.load(Ordering::Acquire),
            canceled: c.canceled.load(Ordering::Acquire),
            protocol_faults: c.protocol_faults.load(Ordering::Acquire),
            workers_live: u64::try_from(self.shared.workers.load(Ordering::Acquire))
                .unwrap_or(u64::MAX),
            in_flight_bytes: self.shared.bytes.load(Ordering::Acquire),
        }
    }

    /// Snapshot of every process ever spawned.
    pub fn ledger(&self) -> Vec<ProcessLedgerEntry> {
        lock(&self.shared.ledger).clone()
    }

    /// Reserves one worker and `bytes` or refuses at once; a partial acquisition is released.
    pub fn admit(&self, bytes: u64) -> Result<Permit, Refused> {
        let shared = &self.shared;
        let max_workers = shared.config.max_workers;
        shared
            .workers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |w| {
                w.checked_add(1).filter(|next| *next <= max_workers)
            })
            .map_err(|_| Refused)?;
        let max_bytes = shared.config.max_in_flight_bytes;
        let reserved = shared
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |b| {
                b.checked_add(bytes).filter(|total| *total <= max_bytes)
            });
        if reserved.is_err() {
            shared.workers.fetch_sub(1, Ordering::AcqRel);
            return Err(Refused);
        }
        Ok(Permit {
            shared: Arc::clone(shared),
            bytes,
        })
    }

    /// Starts a worker and registers it in the ledger; the caller waits for the first frame
    /// ([`Session::wait`]) and classifies a failed handshake itself. From the moment the child exists,
    /// every error path drops the session, which kills and reaps it.
    pub fn start<F, E>(
        &self,
        permit: Permit,
        request: StartRequest<'_>,
        read: ReadFn<F, E>,
        terminal: Option<TerminalFn<F>>,
    ) -> Result<Session<F, E>, StartError>
    where
        F: Send + 'static,
        E: Send + 'static,
    {
        let StartRequest {
            owner_session,
            extra_args,
            keepalive,
            hello_cap,
        } = request;
        let shared = &self.shared;
        let mut command = Command::new(&shared.config.program);
        command
            .args(&shared.config.args)
            .args(extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_clear();
        for (key, value) in &shared.config.env {
            command.env(key, value);
        }
        quiet(&mut command);
        let mut child = command.spawn().map_err(|_| StartError::NotStarted)?;
        bump(&shared.counters.spawned);
        let pid = child.id();
        let ledger_index = {
            let mut ledger = lock(&shared.ledger);
            ledger.push(ProcessLedgerEntry {
                owner_session: owner_session.to_owned(),
                started_unix_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
                pid,
                killed: false,
                reaped: false,
                exit_code: None,
            });
            ledger.len().saturating_sub(1)
        };
        let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();
        let (out, replies_rx) = match shared.config.reply_buffer {
            Some(bound) => {
                let (tx, rx) = mpsc::sync_channel(bound);
                (Out::Bounded(tx), rx)
            }
            None => {
                let (tx, rx) = mpsc::channel();
                (Out::Unbounded(tx), rx)
            }
        };
        let (stdin, stdout) = (child.stdin.take(), child.stdout.take());
        // From here the session owns the child: dropping it on any error path kills and reaps it.
        let mut session = Session {
            child,
            jobs: jobs_tx,
            replies: replies_rx,
            usable: true,
            ledger_index,
            pid,
            shared: Arc::clone(shared),
            finished: None,
            job: None,
            _keepalive: keepalive,
            _permit: permit,
        };
        // The worker is idle until its first request, so the short window before it joins the job is
        // unobservable; a failure to apply the caps fails the start (fail closed).
        if let Some(limits) = shared.config.os_limits {
            session.job = Some(
                WorkerJob::attach(&session.child, limits).map_err(|_| StartError::Setup)?,
            );
        }
        let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
            return Err(StartError::Setup);
        };
        thread::Builder::new()
            .name(shared.config.io_thread_name.clone())
            .spawn(move || io_loop(stdin, stdout, read, terminal, hello_cap, &jobs_rx, &out))
            .map_err(|_| StartError::Setup)?;
        Ok(session)
    }

    /// [`Supervisor::start`] plus the HELLO wait: reads the first frame within the spawn timeout
    /// (cancel polled every [`WAIT_SLICE`]) and classifies a failed handshake.
    pub fn spawn<F, E>(
        &self,
        permit: Permit,
        request: StartRequest<'_>,
        read: ReadFn<F, E>,
        cancel: &CancellationToken,
    ) -> Result<(Session<F, E>, F), SpawnError<E>>
    where
        F: Send + 'static,
        E: Send + 'static,
    {
        if cancel.check().is_err() {
            return Err(SpawnError::Canceled);
        }
        let mut session = self
            .start(permit, request, read, None)
            .map_err(|_| SpawnError::Unavailable)?;
        let wall = after(self.shared.config.spawn_timeout);
        match session.wait(cancel, wall) {
            Waited::Frame(first) => Ok((session, first)),
            Waited::Canceled => Err(session.handshake_failed(Failure::Canceled)),
            Waited::TimedOut | Waited::CpuCap => Err(session.handshake_failed(Failure::Deadline)),
            Waited::Failed(failure) => Err(session.handshake_failed(Failure::Read(failure))),
        }
    }
}

impl<F, E> Session<F, E> {
    fn handshake_failed(&mut self, failure: Failure<E>) -> SpawnError<E> {
        let (classified, done) = self.fail(failure);
        SpawnError::Handshake(classified, done)
    }

    /// Waits for the next reply frame, polling cancel, the CPU cap and the real-time deadline.
    pub fn wait(&self, cancel: &CancellationToken, wall: Instant) -> Waited<F, E> {
        loop {
            match self.replies.recv_timeout(WAIT_SLICE) {
                Ok(Ok(frame)) => return Waited::Frame(frame),
                Ok(Err(failure)) => return Waited::Failed(failure),
                Err(RecvTimeoutError::Disconnected) => return Waited::Failed(ReadFailure::Closed),
                Err(RecvTimeoutError::Timeout) => {
                    if cancel.check().is_err() {
                        return Waited::Canceled;
                    }
                    if self.job.as_ref().is_some_and(WorkerJob::cpu_exceeded) {
                        return Waited::CpuCap;
                    }
                    if Instant::now() >= wall {
                        return Waited::TimedOut;
                    }
                }
            }
        }
    }

    /// Operating-system id of the worker process (also in the ledger).
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// False once the process was terminated or its stream is out of sync.
    pub fn is_usable(&self) -> bool {
        self.usable
    }

    /// Marks the stream out of sync (for example a reply the caller refused mid-batch): the session is
    /// never reused, but the process is not reclaimed until [`Session::terminate`] or drop.
    pub fn retire(&mut self) {
        self.usable = false;
    }

    /// Non-blocking liveness probe of the worker process.
    pub fn heartbeat(&mut self) -> Liveness {
        match self.child.try_wait() {
            Ok(None) => Liveness::Running,
            Ok(Some(status)) => Liveness::Exited {
                exit_code: status.code(),
            },
            Err(_) => Liveness::Unknown,
        }
    }

    /// Usable and still running.
    pub fn is_alive(&mut self) -> bool {
        self.usable && self.heartbeat() == Liveness::Running
    }

    /// Queues one pre-framed request on the I/O thread (reply frames up to `cap` bytes each). False when
    /// the I/O thread is gone.
    pub fn send_request(&self, frame: Vec<u8>, cap: u32) -> bool {
        self.jobs.send(Job { frame, cap }).is_ok()
    }

    /// Re-arms the CPU cap for the next request (no-op without a Job Object).
    pub fn arm_cpu(&mut self, budget: Duration) -> std::io::Result<()> {
        match self.job.as_mut() {
            Some(job) => job.arm_cpu(budget),
            None => Ok(()),
        }
    }

    /// Whether this worker runs under a Job Object.
    pub fn has_os_caps(&self) -> bool {
        self.job.is_some()
    }

    /// Caps the kernel reported since the last call.
    pub fn drain_os_events(&self) -> OsEvents {
        self.job.as_ref().map(WorkerJob::drain).unwrap_or_default()
    }

    /// Writes one request frame and waits for its reply frame (cancel and the deadline are polled).
    /// A failure does NOT reclaim the worker: pass it to [`Session::fail`].
    pub fn exchange(
        &mut self,
        frame: Vec<u8>,
        cap: u32,
        deadline: Duration,
        cancel: &CancellationToken,
    ) -> Result<F, Failure<E>> {
        if cancel.check().is_err() {
            return Err(Failure::Canceled);
        }
        if !self.send_request(frame, cap) {
            return Err(Failure::Read(ReadFailure::Closed));
        }
        match self.wait(cancel, after(deadline)) {
            Waited::Frame(reply) => Ok(reply),
            Waited::Canceled => Err(Failure::Canceled),
            Waited::TimedOut | Waited::CpuCap => Err(Failure::Deadline),
            Waited::Failed(failure) => Err(Failure::Read(failure)),
        }
    }

    /// Reaps the child, killing it if it is still running after `patience`. Idempotent.
    pub fn terminate(&mut self, patience: Duration) -> Terminated {
        self.usable = false;
        if let Some(done) = self.finished {
            return done;
        }
        let until = after(patience);
        let mut done = Terminated {
            killed: false,
            reaped: false,
            exit_code: None,
        };
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    done.reaped = true;
                    done.exit_code = status.code();
                    break;
                }
                Ok(None) if Instant::now() < until => thread::sleep(Duration::from_millis(5)),
                _ => {
                    done.killed = self.child.kill().is_ok();
                    done.reaped = self.child.wait().is_ok();
                    break;
                }
            }
        }
        if done.killed {
            bump(&self.shared.counters.killed);
        }
        if done.reaped {
            bump(&self.shared.counters.reaped);
        }
        if let Some(entry) = lock(&self.shared.ledger).get_mut(self.ledger_index) {
            entry.killed = done.killed;
            entry.reaped = done.reaped;
            entry.exit_code = done.exit_code;
        }
        self.finished = Some(done);
        done
    }

    /// Reclaims the worker for `failure` (kill and reap; a closed stream gets [`EXIT_PATIENCE`] to
    /// report its own exit code) and says how it ended.
    pub fn fail(&mut self, failure: Failure<E>) -> (Classified<E>, Terminated) {
        let counters = &self.shared.counters;
        match failure {
            Failure::Canceled => {
                bump(&counters.canceled);
                (Classified::Canceled, self.terminate(Duration::ZERO))
            }
            Failure::Deadline => {
                bump(&counters.timed_out);
                (Classified::Deadline, self.terminate(Duration::ZERO))
            }
            Failure::Read(ReadFailure::Closed) => {
                bump(&counters.crashed);
                let done = self.terminate(EXIT_PATIENCE);
                (
                    Classified::Crashed {
                        exit_code: done.exit_code,
                    },
                    done,
                )
            }
            Failure::Read(ReadFailure::Failed(error)) => {
                bump(&counters.protocol_faults);
                (Classified::Protocol(error), self.terminate(Duration::ZERO))
            }
        }
    }
}

impl<F, E> Drop for Session<F, E> {
    fn drop(&mut self) {
        // Reached on every error path after the child exists: kill if still running, then reap.
        let _ = self.terminate(Duration::ZERO);
    }
}
