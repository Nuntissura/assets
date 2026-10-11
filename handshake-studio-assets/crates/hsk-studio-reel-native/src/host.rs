//! Host side: `WorkerAv1Provider`, a reel `MediaProvider` (id `av1`) that decodes AV1 in a pool of
//! `reel-native-av1-worker` child processes instead of inside the caller's process.
//!
//! Flow of one segment: the container (ISOBMFF or Matroska) is indexed and delivered by reel's own
//! provider in `OutputMode::Packets` (frame-accurate start, pre-roll from the nearest sync sample); the
//! provider validates the `av1C` record, sends OPEN (configuration + bounds) and then one PACKET per
//! temporal unit to the worker, forwards the pictures it returns to the caller's sink exactly as reel's
//! in-process decoder does, and ends with FINISH. Receipts name the decoder (`rav1d` + version) and are
//! equal to the in-process receipts for the same segment.
//!
//! Supervision (one request = one write + replies read on a per-worker I/O thread, the calling thread only
//! waits on a channel), in order of precedence:
//! * cancel: the worker is killed, the call returns `Canceled`;
//! * deadlines: every request (a batch = one packet and the pictures it releases) has `batch_deadline`
//!   and the whole segment has `segment_deadline`, both on the real monotonic clock: the worker is
//!   killed, `decoder_deadline`;
//! * output cap: a reply frame above the cap derived from the DECLARED picture dimensions (container
//!   track size and `av1C` chroma/bit depth, never above `Limits::max_chunk_bytes`) kills the worker,
//!   `decoder_output_too_large`; the same declared size bounds the decoder's own allocation;
//! * crash: end of stream (abort, native crash, external kill) returns `decoder_crashed` with the exit
//!   code and the worker is reaped. The provider stays usable; the next call starts a fresh worker, but
//!   at most `max_spawns` workers start per `spawn_window` (`worker_respawn_limited` beyond that).
//!
//! Only processes this provider spawned are ever killed (it holds the `Child` handles). Workers get an
//! empty environment (no inherited secrets), a null stderr, piped stdin/stdout and, on Windows,
//! `CREATE_NO_WINDOW`, so they never show a console window or take foreground focus.
//!
//! NOT_PROVEN: an operating-system memory/CPU cap for the worker (Windows Job Object) needs `unsafe`
//! FFI, which this crate forbids; the declared-dimension decoder limit, the output cap and the
//! wall-clock kill are the enforced bounds.
use crate::wire::{self, OpenRequest, Reply, WireError, WireFrame};
use crate::worker::WorkerTestFault;
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_reel::av1::{self, Av1Config, DECODER_ID, decoder_version, parse_av1c};
use hsk_studio_reel::{
    Chunk, ChunkKind, ChunkSink, CodecDescriptor, ColorTag, Disposition, ISOBMFF_PROVIDER_ID,
    IsobmffProvider, Limits, MATROSKA_PROVIDER_ID, MatroskaProvider, MediaIndex, MediaProvider,
    MkvIndex, OutputMode, ReadSeek, ReelError, SegmentReceipt, SegmentRequest, StreamLayout,
    VideoFormat, build_index, build_matroska_index, detect_type, fourcc_string,
};
use std::{
    collections::VecDeque,
    io::SeekFrom,
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

/// Provider id this provider registers under in a reel `Registry`.
pub const AV1_PROVIDER_ID: &str = "av1";
/// File name of the worker executable (without the platform suffix).
pub const WORKER_PROGRAM: &str = "reel-native-av1-worker";
/// How often a waiting host thread re-checks cancel and the wall-clock deadlines.
const WAIT_SLICE: Duration = Duration::from_millis(5);
/// How long a worker that closed its stdout gets to exit by itself (to read its true exit code).
const EXIT_PATIENCE: Duration = Duration::from_millis(500);

/// Win32 `CREATE_NO_WINDOW` process creation flag (0x08000000, Process Creation Flags, Win32 docs).
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Worker-pool policy.
#[derive(Clone, Debug)]
pub struct Av1WorkerOptions {
    /// Worker executable. `None`: `reel-native-av1-worker` next to the running executable (or one
    /// directory above it, for test and example binaries).
    pub program: Option<PathBuf>,
    /// Most worker processes alive at once (idle plus busy); a call beyond that is refused at once.
    pub max_workers: usize,
    /// Bounded respawn rate: at most this many workers start per `spawn_window`.
    pub max_spawns: u32,
    pub spawn_window: Duration,
    /// Time a new worker gets to complete its HELLO handshake.
    pub spawn_timeout: Duration,
    /// Time one request (a packet and the pictures it releases, OPEN, FINISH) may take.
    pub batch_deadline: Duration,
    /// Time a whole segment may take.
    pub segment_deadline: Duration,
    #[doc(hidden)]
    pub test_fault: WorkerTestFault,
}

impl Default for Av1WorkerOptions {
    fn default() -> Self {
        Self {
            program: None,
            max_workers: 2,
            max_spawns: 16,
            spawn_window: Duration::from_secs(10),
            spawn_timeout: Duration::from_secs(10),
            batch_deadline: Duration::from_secs(30),
            segment_deadline: Duration::from_secs(300),
            test_fault: WorkerTestFault::None,
        }
    }
}

impl Av1WorkerOptions {
    pub fn with_worker_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = Some(program.into());
        self
    }
}

/// Why a provider could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Av1ConfigError {
    /// A pool bound or deadline is zero.
    PolicyInvalid,
    /// No worker executable at the configured or discovered location.
    WorkerProgramMissing,
}

impl std::fmt::Display for Av1ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::PolicyInvalid => "av1-worker-policy-invalid",
            Self::WorkerProgramMissing => "av1-worker-program-missing",
        })
    }
}

impl std::error::Error for Av1ConfigError {}

/// A contained failure of the worker process (the provider stays usable after every one of them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Av1WorkerFault {
    /// The worker ended without a reply: abort, native crash or external kill. `exit_code` is the
    /// platform value when the process exited by itself.
    DecoderCrashed { exit_code: Option<i32> },
    /// A request or the segment exceeded its deadline; the worker was killed.
    DecoderDeadline,
    /// A reply frame above the cap derived from the declared dimensions; the worker was killed.
    OutputTooLarge,
    /// Wrong kind, id, count or content in a reply; the worker was killed.
    Protocol,
    /// The worker speaks another protocol version or decoder than this host was built for.
    VersionMismatch,
    /// The worker process could not be started or did not complete HELLO.
    SpawnFailed,
    /// More than `max_spawns` workers would have started within `spawn_window`.
    RespawnLimited,
    /// `max_workers` workers are alive and none is idle.
    PoolBusy,
}

impl Av1WorkerFault {
    /// Stable wire code.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::DecoderCrashed { .. } => "decoder_crashed",
            Self::DecoderDeadline => "decoder_deadline",
            Self::OutputTooLarge => "decoder_output_too_large",
            Self::Protocol => "worker_protocol",
            Self::VersionMismatch => "worker_version_mismatch",
            Self::SpawnFailed => "worker_spawn_failed",
            Self::RespawnLimited => "worker_respawn_limited",
            Self::PoolBusy => "worker_pool_busy",
        }
    }
}

/// Failure of one `decode_av1` call: a typed media error, or a contained worker fault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Av1Error {
    Reel(ReelError),
    Worker(Av1WorkerFault),
}

impl Av1Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Reel(error) => error.code(),
            Self::Worker(fault) => fault.code(),
        }
    }

    /// The reel-vocabulary form used by the `MediaProvider` port (reel's `ReelError` is closed, so a
    /// worker fault travels as a class plus its stable fault code in `what`). UNVERIFIED mapping: the
    /// reel owner has not named a variant for contained worker faults.
    pub fn into_reel(self) -> ReelError {
        match self {
            Self::Reel(error) => error,
            Self::Worker(fault) => match fault {
                Av1WorkerFault::DecoderCrashed { .. } | Av1WorkerFault::Protocol => {
                    ReelError::Corrupt { what: fault.code() }
                }
                Av1WorkerFault::VersionMismatch => ReelError::Unsupported { what: fault.code() },
                Av1WorkerFault::SpawnFailed => ReelError::CodecUnavailable {
                    codec: AV1_PROVIDER_ID.to_owned(),
                },
                Av1WorkerFault::DecoderDeadline
                | Av1WorkerFault::OutputTooLarge
                | Av1WorkerFault::RespawnLimited
                | Av1WorkerFault::PoolBusy => ReelError::LimitExceeded { what: fault.code() },
            },
        }
    }
}

impl From<ReelError> for Av1Error {
    fn from(error: ReelError) -> Self {
        Self::Reel(error)
    }
}

impl std::fmt::Display for Av1Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reel(error) => error.fmt(f),
            Self::Worker(fault) => f.write_str(fault.code()),
        }
    }
}

impl std::error::Error for Av1Error {}

/// Monotonic process counters of one worker pool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Av1WorkerStats {
    pub spawned: u64,
    /// Requests (OPEN, PACKET, FINISH) written to workers.
    pub requests: u64,
    pub spawn_failed: u64,
    /// Spawns refused by the bounded respawn rate.
    pub spawn_denied: u64,
    pub crashed: u64,
    pub protocol_faults: u64,
    pub killed_deadline: u64,
    pub killed_canceled: u64,
    pub killed_oversize: u64,
    /// Workers alive right now (idle plus busy); every other worker was reaped.
    pub alive: u64,
    /// Exit code of the most recent crashed worker (platform value; 70 is the panic-hook exit).
    pub last_crash_exit_code: Option<i32>,
}

#[derive(Default)]
struct Counters {
    spawned: AtomicU64,
    requests: AtomicU64,
    spawn_failed: AtomicU64,
    spawn_denied: AtomicU64,
    crashed: AtomicU64,
    protocol_faults: AtomicU64,
    killed_deadline: AtomicU64,
    killed_canceled: AtomicU64,
    killed_oversize: AtomicU64,
    last_crash_exit_code: Mutex<Option<i32>>,
}

fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::AcqRel);
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Job {
    frame: Vec<u8>,
    cap: usize,
}

type Frames = Result<Vec<u8>, WireError>;

enum Waited {
    Frame(Vec<u8>),
    Canceled,
    TimedOut,
    Failed(WireError),
}

/// Counts one live worker process; released only after its `Session` has been reaped.
struct Permit(Arc<AtomicUsize>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One live worker process plus the I/O thread that owns its pipes.
struct Session {
    child: Child,
    jobs: mpsc::Sender<Job>,
    frames: mpsc::Receiver<Frames>,
    /// False once the process was terminated or its stream is out of sync: never reused.
    usable: bool,
    _permit: Permit,
}

impl Session {
    fn is_alive(&mut self) -> bool {
        self.usable && matches!(self.child.try_wait(), Ok(None))
    }

    /// Waits for the next reply frame, polling cancel and the real-time deadline.
    fn wait(&self, cancel: &CancellationToken, wall: Instant) -> Waited {
        loop {
            match self.frames.recv_timeout(WAIT_SLICE) {
                Ok(Ok(frame)) => return Waited::Frame(frame),
                Ok(Err(error)) => return Waited::Failed(error),
                Err(RecvTimeoutError::Disconnected) => return Waited::Failed(WireError::Closed),
                Err(RecvTimeoutError::Timeout) => {
                    if cancel.check().is_err() {
                        return Waited::Canceled;
                    }
                    if Instant::now() >= wall {
                        return Waited::TimedOut;
                    }
                }
            }
        }
    }

    /// Reaps the child, killing it if it is still running after `patience`. Returns whether the
    /// process is provably gone and, if it exited by itself, its exit code.
    fn terminate(&mut self, patience: Duration) -> (bool, Option<i32>) {
        self.usable = false;
        let until = Instant::now() + patience;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return (true, status.code()),
                Ok(None) if Instant::now() < until => thread::sleep(Duration::from_millis(5)),
                _ => break,
            }
        }
        let _ = self.child.kill();
        (self.child.wait().is_ok(), None)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.terminate(Duration::ZERO);
    }
}

/// Owns the worker's pipes: HELLO first, then per job one write and every reply up to the terminator.
fn io_loop(
    mut stdin: ChildStdin,
    mut stdout: ChildStdout,
    jobs: &mpsc::Receiver<Job>,
    out: &mpsc::SyncSender<Frames>,
) {
    let hello = wire::read_frame(&mut stdout, wire::HELLO_CAP);
    let failed = hello.is_err();
    if out.send(hello).is_err() || failed {
        return;
    }
    while let Ok(job) = jobs.recv() {
        if wire::write_frame(&mut stdin, &job.frame).is_err() {
            let _ = out.send(Err(WireError::Closed));
            return;
        }
        loop {
            match wire::read_frame(&mut stdout, job.cap) {
                Ok(frame) => {
                    let terminal = wire::is_terminal(&frame);
                    if out.send(Ok(frame)).is_err() {
                        return;
                    }
                    if terminal {
                        break;
                    }
                }
                Err(error) => {
                    let _ = out.send(Err(error));
                    return;
                }
            }
        }
    }
}

#[cfg(windows)]
fn quiet(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn quiet(_command: &mut Command) {}

fn discover_program() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let name = format!("{WORKER_PROGRAM}{}", std::env::consts::EXE_SUFFIX);
    let dir = exe.parent()?;
    [
        Some(dir.join(&name)),
        dir.parent().map(|parent| parent.join(&name)),
    ]
    .into_iter()
    .flatten()
    .find(|candidate| candidate.is_file())
}

/// Why a request ended without a usable terminal reply.
enum Failure {
    Canceled,
    Deadline,
    Oversize,
    Protocol,
    Closed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    Opened,
    BatchEnd,
}

/// One request to a worker: what to send, the reply cap, and the terminal reply that ends it.
struct Call {
    id: u64,
    frame: Vec<u8>,
    cap: usize,
    want: Want,
}

/// Per-segment time and cancel context shared by every request of the segment.
struct Clock<'a> {
    cancel: &'a CancellationToken,
    segment_wall: Instant,
}

struct Pool {
    program: PathBuf,
    max_workers: usize,
    max_spawns: usize,
    spawn_window: Duration,
    spawn_timeout: Duration,
    batch_deadline: Duration,
    segment_deadline: Duration,
    fault_env: Option<&'static str>,
    live: Arc<AtomicUsize>,
    idle: Mutex<Vec<Session>>,
    spawn_log: Mutex<VecDeque<Instant>>,
    request_ids: AtomicU64,
    counters: Counters,
}

impl Pool {
    fn next_id(&self) -> u64 {
        self.request_ids.fetch_add(1, Ordering::AcqRel)
    }

    fn admit_spawn(&self) -> Result<(), Av1WorkerFault> {
        let now = Instant::now();
        let mut log = lock(&self.spawn_log);
        while log
            .front()
            .is_some_and(|started| now.duration_since(*started) > self.spawn_window)
        {
            log.pop_front();
        }
        if log.len() >= self.max_spawns {
            bump(&self.counters.spawn_denied);
            return Err(Av1WorkerFault::RespawnLimited);
        }
        log.push_back(now);
        Ok(())
    }

    fn spawn(&self) -> Result<Session, Av1WorkerFault> {
        let previous = self.live.fetch_add(1, Ordering::AcqRel);
        let permit = Permit(Arc::clone(&self.live));
        if previous >= self.max_workers {
            return Err(Av1WorkerFault::PoolBusy);
        }
        self.admit_spawn()?;
        let spawn_failed = || {
            bump(&self.counters.spawn_failed);
            Av1WorkerFault::SpawnFailed
        };
        let mut command = Command::new(&self.program);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_clear();
        if let Some(value) = self.fault_env {
            command.env(WorkerTestFault::ENV, value);
        }
        quiet(&mut command);
        let mut child = command.spawn().map_err(|_| spawn_failed())?;
        bump(&self.counters.spawned);
        let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();
        let (frames_tx, frames_rx) = mpsc::sync_channel::<Frames>(1);
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(spawn_failed());
        };
        // From here the session owns the child: dropping it on any error path kills and reaps it.
        let mut session = Session {
            child,
            jobs: jobs_tx,
            frames: frames_rx,
            usable: true,
            _permit: permit,
        };
        let io_thread = thread::Builder::new()
            .name("reel-native-av1-worker-io".to_owned())
            .spawn(move || io_loop(stdin, stdout, &jobs_rx, &frames_tx));
        if io_thread.is_err() {
            return Err(spawn_failed());
        }
        match session.frames.recv_timeout(self.spawn_timeout) {
            Ok(Ok(hello)) => match wire::decode_hello(&hello) {
                Ok(hello)
                    if hello.protocol == wire::PROTOCOL_VERSION
                        && hello.decoder_id == DECODER_ID
                        && hello.decoder_version == decoder_version() =>
                {
                    Ok(session)
                }
                Ok(_) => {
                    session.terminate(Duration::ZERO);
                    Err(Av1WorkerFault::VersionMismatch)
                }
                Err(_) => {
                    bump(&self.counters.protocol_faults);
                    session.terminate(Duration::ZERO);
                    Err(Av1WorkerFault::Protocol)
                }
            },
            _ => {
                session.terminate(Duration::ZERO);
                Err(spawn_failed())
            }
        }
    }

    fn checkout(&self) -> Result<Session, Av1WorkerFault> {
        loop {
            let idle = lock(&self.idle).pop();
            let Some(mut session) = idle else {
                return self.spawn();
            };
            if session.is_alive() {
                return Ok(session);
            }
            session.terminate(Duration::ZERO);
        }
    }

    fn checkin(&self, mut session: Session) {
        if !session.is_alive() {
            return;
        }
        let mut idle = lock(&self.idle);
        if idle.len() < self.max_workers {
            idle.push(session);
        }
    }

    /// Ends a request that produced no usable terminal reply: the worker is terminated and reaped
    /// (a worker that only closed its stream gets `EXIT_PATIENCE` to report its own exit code).
    fn fail(&self, session: &mut Session, failure: Failure) -> Av1Error {
        let counters = &self.counters;
        match failure {
            Failure::Canceled => {
                bump(&counters.killed_canceled);
                session.terminate(Duration::ZERO);
                Av1Error::Reel(ReelError::Canceled)
            }
            Failure::Deadline => {
                bump(&counters.killed_deadline);
                session.terminate(Duration::ZERO);
                Av1Error::Worker(Av1WorkerFault::DecoderDeadline)
            }
            Failure::Oversize => {
                bump(&counters.killed_oversize);
                session.terminate(Duration::ZERO);
                Av1Error::Worker(Av1WorkerFault::OutputTooLarge)
            }
            Failure::Protocol => {
                bump(&counters.protocol_faults);
                session.terminate(Duration::ZERO);
                Av1Error::Worker(Av1WorkerFault::Protocol)
            }
            Failure::Closed => {
                bump(&counters.crashed);
                let (_, exit_code) = session.terminate(EXIT_PATIENCE);
                *lock(&counters.last_crash_exit_code) = exit_code;
                Av1Error::Worker(Av1WorkerFault::DecoderCrashed { exit_code })
            }
        }
    }

    /// Sends one request and processes its replies: every FRAME goes to `on_frame` as it arrives (one
    /// picture resident at a time), the terminal reply must be `want`. A typed decoder ERROR leaves the
    /// worker usable (it dropped its decoder); every other failure terminates it.
    fn exchange(
        &self,
        session: &mut Session,
        call: Call,
        clock: &Clock<'_>,
        on_frame: &mut dyn FnMut(WireFrame) -> Result<(), ReelError>,
    ) -> Result<(), Av1Error> {
        let Call {
            id,
            frame,
            cap,
            want,
        } = call;
        let wall = (Instant::now() + self.batch_deadline).min(clock.segment_wall);
        if session.jobs.send(Job { frame, cap }).is_err() {
            return Err(self.fail(session, Failure::Closed));
        }
        bump(&self.counters.requests);
        let mut frames = 0_usize;
        loop {
            let bytes = match session.wait(clock.cancel, wall) {
                Waited::Frame(bytes) => bytes,
                Waited::Canceled => return Err(self.fail(session, Failure::Canceled)),
                Waited::TimedOut => return Err(self.fail(session, Failure::Deadline)),
                Waited::Failed(WireError::TooLarge) => {
                    return Err(self.fail(session, Failure::Oversize));
                }
                Waited::Failed(WireError::Malformed) => {
                    return Err(self.fail(session, Failure::Protocol));
                }
                Waited::Failed(WireError::Closed) => {
                    return Err(self.fail(session, Failure::Closed));
                }
            };
            match wire::decode_reply(&bytes, id) {
                Ok(Reply::Frame(picture)) => {
                    frames += 1;
                    if frames > wire::MAX_FRAMES_PER_BATCH {
                        return Err(self.fail(session, Failure::Protocol));
                    }
                    if let Err(error) = on_frame(picture) {
                        // The rest of this batch is still in the pipe: the stream is out of sync.
                        session.terminate(Duration::ZERO);
                        return Err(Av1Error::Reel(error));
                    }
                }
                Ok(Reply::Opened) if want == Want::Opened && frames == 0 => return Ok(()),
                Ok(Reply::BatchEnd(count))
                    if want == Want::BatchEnd && usize::try_from(count) == Ok(frames) =>
                {
                    return Ok(());
                }
                Ok(Reply::Error(error)) => return Err(Av1Error::Reel(error)),
                _ => return Err(self.fail(session, Failure::Protocol)),
            }
        }
    }
}

/// Returns a session to the idle pool when the segment ends, unless a failure made it unusable.
struct Lease<'a> {
    pool: &'a Pool,
    session: Option<Session>,
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            self.pool.checkin(session);
        }
    }
}

/// Process-isolated AV1 decoder (host side). Cheap to clone: clones share one worker pool.
#[derive(Clone)]
pub struct WorkerAv1Provider {
    pool: Arc<Pool>,
}

impl WorkerAv1Provider {
    pub fn new(options: Av1WorkerOptions) -> Result<Self, Av1ConfigError> {
        if options.max_workers == 0
            || options.max_spawns == 0
            || options.spawn_window.is_zero()
            || options.spawn_timeout.is_zero()
            || options.batch_deadline.is_zero()
            || options.segment_deadline.is_zero()
        {
            return Err(Av1ConfigError::PolicyInvalid);
        }
        let program = options
            .program
            .clone()
            .or_else(discover_program)
            .filter(|path| path.is_file())
            .ok_or(Av1ConfigError::WorkerProgramMissing)?;
        Ok(Self {
            pool: Arc::new(Pool {
                program,
                max_workers: options.max_workers,
                max_spawns: usize::try_from(options.max_spawns).unwrap_or(usize::MAX),
                spawn_window: options.spawn_window,
                spawn_timeout: options.spawn_timeout,
                batch_deadline: options.batch_deadline,
                segment_deadline: options.segment_deadline,
                fault_env: options.test_fault.env_value(),
                live: Arc::new(AtomicUsize::new(0)),
                idle: Mutex::new(Vec::new()),
                spawn_log: Mutex::new(VecDeque::new()),
                request_ids: AtomicU64::new(1),
                counters: Counters::default(),
            }),
        })
    }

    pub fn stats(&self) -> Av1WorkerStats {
        let c = &self.pool.counters;
        Av1WorkerStats {
            spawned: c.spawned.load(Ordering::Acquire),
            requests: c.requests.load(Ordering::Acquire),
            spawn_failed: c.spawn_failed.load(Ordering::Acquire),
            spawn_denied: c.spawn_denied.load(Ordering::Acquire),
            crashed: c.crashed.load(Ordering::Acquire),
            protocol_faults: c.protocol_faults.load(Ordering::Acquire),
            killed_deadline: c.killed_deadline.load(Ordering::Acquire),
            killed_canceled: c.killed_canceled.load(Ordering::Acquire),
            killed_oversize: c.killed_oversize.load(Ordering::Acquire),
            alive: u64::try_from(self.pool.live.load(Ordering::Acquire)).unwrap_or(u64::MAX),
            last_crash_exit_code: *lock(&c.last_crash_exit_code),
        }
    }

    /// Decodes one AV1 segment in a worker process. Same request, sink and receipt vocabulary as reel's
    /// in-process decode (`req.codec` is `"av1"`, `req.output` is `Decoded`); the typed result keeps the
    /// contained worker faults distinct from media errors. Pictures already handed to `sink` before an
    /// error stay delivered: an `Err` means the segment is incomplete.
    pub fn decode_av1(
        &self,
        src: &mut dyn ReadSeek,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
    ) -> Result<SegmentReceipt, Av1Error> {
        check_cancel(cancel)?;
        if req.codec != AV1_PROVIDER_ID {
            return Err(ReelError::CodecUnavailable {
                codec: req.codec.clone(),
            }
            .into());
        }
        if req.output != OutputMode::Decoded {
            return Err(ReelError::Unsupported {
                what: "av1-decoded-output-only",
            }
            .into());
        }
        if req.count == 0 {
            return Err(ReelError::Unsupported { what: "zero-count" }.into());
        }
        let file_len = verify_grant(src, req)?;
        let head = read_head(src)?;
        let plan = Plan::new(src, &head, file_len, req, limits, cancel)?;
        // Exactly what reel's in-process decoder validates before its first packet.
        let parsed = if plan.config.is_empty() {
            None
        } else {
            Some(parse_av1c(&plan.config)?)
        };
        let frame_cap = declared_frame_cap(parsed.as_ref(), plan.width, plan.height, limits);
        let open = OpenRequest {
            max_chunk_bytes: u64::try_from(decoder_budget(plan.width, plan.height, limits))
                .unwrap_or(u64::MAX),
            color: plan.color,
            config: plan.config.clone(),
        };
        let pool = &*self.pool;
        let clock = Clock {
            cancel,
            segment_wall: Instant::now() + pool.segment_deadline,
        };
        let mut lease = Lease {
            pool,
            session: Some(pool.checkout().map_err(Av1Error::Worker)?),
        };
        let session = lease.session.as_mut().expect("lease holds the session");
        let mut pump = Pump {
            pool,
            session,
            clock: &clock,
            reply_cap: frame_cap + wire::REPLY_SLACK,
            max_packet_bytes: limits.max_chunk_bytes.min(wire::MAX_REQUEST_FRAME - 64),
            fwd: Forward {
                pending: VecDeque::new(),
                sink,
                max_chunk_bytes: limits.max_chunk_bytes,
                format: None,
                delivered: 0,
                packed: Vec::new(),
            },
            fault: None,
        };
        pump.open(&open)?;
        let delivered = match &plan.demux {
            Demux::Iso(index) => IsobmffProvider.decode_packets(
                src,
                index,
                &plan.packet_request(req),
                limits,
                cancel,
                &mut pump,
            ),
            Demux::Mkv(index) => MatroskaProvider.decode_packets(
                src,
                index,
                &plan.packet_request(req),
                limits,
                cancel,
                &mut pump,
            ),
        };
        let mut receipt = match delivered {
            Ok(receipt) => receipt,
            Err(error) => return Err(pump.fault.take().unwrap_or(Av1Error::Reel(error))),
        };
        pump.finish()?;
        receipt.bytes_read += plan.index_bytes;
        receipt.decoder_id = DECODER_ID;
        receipt.decoder_version = decoder_version();
        receipt.video = pump.fwd.format;
        receipt.actual.units = pump.fwd.delivered;
        if pump.fwd.delivered < req.count {
            receipt.disposition = Disposition::Partial;
            receipt.complete = false;
        }
        Ok(receipt)
    }
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), ReelError> {
    cancel.check().map_err(|_| ReelError::Canceled)
}

/// Same order as reel's providers: revision, then live length, before any payload read.
fn verify_grant(src: &mut dyn ReadSeek, req: &SegmentRequest) -> Result<u64, ReelError> {
    if req.grant.revision != req.expected_revision {
        return Err(ReelError::StaleGrant);
    }
    let len = src.seek(SeekFrom::End(0))?;
    if len != req.grant.expected_len {
        return Err(ReelError::StaleGrant);
    }
    src.seek(SeekFrom::Start(0))?;
    Ok(len)
}

fn read_head(src: &mut dyn ReadSeek) -> Result<Vec<u8>, ReelError> {
    let mut head = [0_u8; 32];
    src.seek(SeekFrom::Start(0))?;
    let mut filled = 0;
    while filled < head.len() {
        let n = std::io::Read::read(src, &mut head[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    src.seek(SeekFrom::Start(0))?;
    Ok(head[..filled].to_vec())
}

enum Demux {
    Iso(MediaIndex),
    Mkv(MkvIndex),
}

/// The container side of one segment: which track carries AV1 and what the decoder needs to know.
struct Plan {
    demux: Demux,
    track: u32,
    config: Vec<u8>,
    color: ColorTag,
    width: u32,
    height: u32,
    index_bytes: u64,
}

impl Plan {
    fn new(
        src: &mut dyn ReadSeek,
        head: &[u8],
        file_len: u64,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<Self, ReelError> {
        if IsobmffProvider.sniff(head) {
            let index = build_index(src, file_len, limits, cancel)?;
            let track = match req.track {
                Some(id) => index.track(id).ok_or(ReelError::Unsupported {
                    what: "track-not-found",
                })?,
                None => index.default_track().ok_or(ReelError::Unsupported {
                    what: "no-audio-or-video-track",
                })?,
            };
            if track.codec != *b"av01" {
                return Err(ReelError::CodecUnavailable {
                    codec: fourcc_string(track.codec),
                });
            }
            let plan = Self {
                track: track.track_id,
                config: track.codec_config.clone(),
                color: track.color,
                width: track.width,
                height: track.height,
                index_bytes: index.bytes_read,
                demux: Demux::Iso(index),
            };
            Ok(plan)
        } else if MatroskaProvider.sniff(head) {
            let index = build_matroska_index(src, file_len, limits, cancel)?;
            let track = match req.track {
                Some(number) => index.track(number).ok_or(ReelError::Unsupported {
                    what: "track-not-found",
                })?,
                None => index.default_track().ok_or(ReelError::Unsupported {
                    what: "no-audio-or-video-track",
                })?,
            };
            if track.info.codec_id != "V_AV1" {
                return Err(ReelError::CodecUnavailable {
                    codec: track.info.codec_id.clone(),
                });
            }
            if track.info.content_encoded {
                return Err(ReelError::Unsupported {
                    what: "content-encoding",
                });
            }
            Ok(Self {
                track: track.info.number,
                config: track.info.codec_private.clone(),
                color: ColorTag::Untagged,
                width: track.info.width,
                height: track.info.height,
                index_bytes: index.bytes_read,
                demux: Demux::Mkv(index),
            })
        } else {
            Err(ReelError::ImportUnsupported {
                detected: detect_type(head).to_owned(),
            })
        }
    }

    /// The same request as the caller's, retargeted at the container provider's packet delivery.
    fn packet_request(&self, req: &SegmentRequest) -> SegmentRequest {
        let mut packets = req.clone();
        packets.codec = match self.demux {
            Demux::Iso(_) => ISOBMFF_PROVIDER_ID,
            Demux::Mkv(_) => MATROSKA_PROVIDER_ID,
        }
        .to_owned();
        packets.output = OutputMode::Packets;
        packets.track = Some(self.track);
        packets
    }
}

/// Largest packed picture the declared dimensions allow (container track size, `av1C` chroma and bit
/// depth; worst case 4:4:4 16-bit when the record is absent), never above the caller's chunk limit.
fn declared_frame_cap(
    config: Option<&Av1Config>,
    width: u32,
    height: u32,
    limits: &Limits,
) -> usize {
    if width == 0 || height == 0 {
        return limits.max_chunk_bytes;
    }
    let (w, h) = (u64::from(width), u64::from(height));
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let samples = match config {
        Some(c) if c.monochrome => w * h,
        Some(c) if c.subsampling_x && c.subsampling_y => w * h + 2 * cw * ch,
        Some(c) if c.subsampling_x => w * h + 2 * cw * h,
        _ => 3 * w * h,
    };
    let bytes_per_sample = match config {
        Some(c) if !c.high_bitdepth => 1,
        _ => 2,
    };
    usize::try_from(samples * bytes_per_sample)
        .unwrap_or(usize::MAX)
        .min(limits.max_chunk_bytes)
}

/// The decoder-side picture bound sent to the worker: rav1d's pixel limit is this divided by six (the
/// worst packed bytes per pixel), so a stream that declares more pixels than its container is refused
/// by the decoder before it allocates the picture.
fn decoder_budget(width: u32, height: u32, limits: &Limits) -> usize {
    if width == 0 || height == 0 {
        return limits.max_chunk_bytes;
    }
    usize::try_from(u64::from(width) * u64::from(height) * 6)
        .unwrap_or(usize::MAX)
        .min(limits.max_chunk_bytes)
}

#[derive(Clone, Copy)]
struct PacketMeta {
    index: u64,
    pts: i64,
    duration: Ticks,
    sync: bool,
    preroll: bool,
    trim_start: u64,
}

/// Forwards returned pictures to the caller's sink with the same matching and bookkeeping as reel's
/// in-process `Pump`: a picture belongs to the packet whose tag it carries, pre-roll pictures are
/// dropped, and the first delivered format must not change inside one segment.
struct Forward<'a> {
    pending: VecDeque<PacketMeta>,
    sink: &'a mut dyn ChunkSink,
    max_chunk_bytes: usize,
    format: Option<VideoFormat>,
    delivered: u64,
    packed: Vec<u8>,
}

impl Forward<'_> {
    fn forward(&mut self, frame: WireFrame) -> Result<(), ReelError> {
        let Some(at) = self.pending.iter().position(|m| m.index == frame.tag) else {
            return Ok(()); // configuration units and dropped packets carry no presentable frame
        };
        let meta = self.pending[at];
        self.pending.drain(..=at);
        if meta.preroll {
            return Ok(()); // decoded only to reach the requested frames
        }
        match self.format {
            None => self.format = Some(frame.format),
            Some(first) if first != frame.format => {
                return Err(ReelError::Unsupported {
                    what: "av1-format-change-mid-segment",
                });
            }
            Some(_) => {}
        }
        self.packed.clear();
        for plane in &frame.planes {
            self.packed.extend_from_slice(plane);
        }
        if self.packed.len() > self.max_chunk_bytes {
            return Err(ReelError::LimitExceeded {
                what: "av1-frame-bytes",
            });
        }
        self.sink.accept(Chunk {
            kind: ChunkKind::VideoFrame,
            pts: meta.pts,
            duration: meta.duration,
            index: meta.index,
            units: 1,
            sync: meta.sync,
            preroll: false,
            trim_start: meta.trim_start,
            bytes: &self.packed,
        })?;
        self.delivered += 1;
        Ok(())
    }
}

/// The chunk sink that container packet delivery feeds: each packet becomes one worker request.
struct Pump<'a, 'b> {
    pool: &'a Pool,
    session: &'a mut Session,
    clock: &'a Clock<'a>,
    reply_cap: usize,
    max_packet_bytes: usize,
    fwd: Forward<'b>,
    /// A worker fault raised inside `accept` (the port can only return a `ReelError` there).
    fault: Option<Av1Error>,
}

impl Pump<'_, '_> {
    fn request(&mut self, frame: Vec<u8>, id: u64, want: Want) -> Result<(), Av1Error> {
        let Self {
            pool,
            session,
            clock,
            reply_cap,
            fwd,
            ..
        } = self;
        let call = Call {
            id,
            frame,
            cap: *reply_cap,
            want,
        };
        pool.exchange(session, call, clock, &mut |picture| fwd.forward(picture))
    }

    fn open(&mut self, open: &OpenRequest) -> Result<(), Av1Error> {
        let id = self.pool.next_id();
        self.request(wire::encode_open(id, open), id, Want::Opened)
    }

    fn finish(&mut self) -> Result<(), Av1Error> {
        let id = self.pool.next_id();
        self.request(wire::encode_finish(id), id, Want::BatchEnd)
    }
}

impl ChunkSink for Pump<'_, '_> {
    fn accept(&mut self, chunk: Chunk<'_>) -> Result<(), ReelError> {
        check_cancel(self.clock.cancel)?;
        if chunk.bytes.len() > self.max_packet_bytes {
            return Err(ReelError::LimitExceeded {
                what: "av1-packet-bytes",
            });
        }
        self.fwd.pending.push_back(PacketMeta {
            index: chunk.index,
            pts: chunk.pts,
            duration: chunk.duration,
            sync: chunk.sync,
            preroll: chunk.preroll,
            trim_start: chunk.trim_start,
        });
        if chunk.bytes.is_empty() {
            return Ok(()); // an empty unit decodes to nothing, as in-process
        }
        let id = self.pool.next_id();
        let frame = wire::encode_packet(id, chunk.index, chunk.bytes);
        match self.request(frame, id, Want::BatchEnd) {
            Ok(()) => Ok(()),
            Err(Av1Error::Reel(error)) => Err(error),
            Err(fault) => {
                self.fault = Some(fault);
                Err(ReelError::Io(std::io::ErrorKind::Other))
            }
        }
    }
}

impl MediaProvider for WorkerAv1Provider {
    fn descriptor(&self) -> CodecDescriptor {
        av1::descriptor()
    }

    fn probe(
        &self,
        src: &mut dyn ReadSeek,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<StreamLayout, ReelError> {
        let head = read_head(src)?;
        if IsobmffProvider.sniff(&head) {
            IsobmffProvider.probe(src, limits, cancel)
        } else if MatroskaProvider.sniff(&head) {
            MatroskaProvider.probe(src, limits, cancel)
        } else {
            Err(ReelError::ImportUnsupported {
                detected: detect_type(&head).to_owned(),
            })
        }
    }

    fn decode_segment(
        &self,
        src: &mut dyn ReadSeek,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
    ) -> Result<SegmentReceipt, ReelError> {
        self.decode_av1(src, req, limits, cancel, sink)
            .map_err(Av1Error::into_reel)
    }
}
