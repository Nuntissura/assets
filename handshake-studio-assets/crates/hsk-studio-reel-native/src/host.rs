//! Host side: `WorkerAv1Provider`, a reel `MediaProvider` (id `av1`) that decodes AV1 in a pool of
//! `reel-native-av1-worker` child processes instead of inside the caller's process.
//!
//! Routing: `WorkerAv1Provider` implements reel's `ExternalVideoDecoder`. Registered on a reel `Registry`
//! (`Registry::with_video_decoder(Arc::new(provider))`) it receives the AV1 of every UNTRUSTED source that
//! the `isobmff`/`matroska` providers are asked to decode; reel's packet pump does the container work
//! (index, frame-accurate start, pre-roll from the nearest sync sample), picture matching, format-change
//! refusal and receipt fields, so receipts are equal to the in-process receipts for the same segment. This
//! side only runs the worker session: OPEN (configuration + bounds), one PACKET per temporal unit,
//! FINISH. `decode_av1` (provider id `av1`) is the same path with the typed `Av1Error` result.
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
//! Decoder identity: this host does not link the decoder (reel without its `av1` feature). It accepts a
//! worker only if its HELLO names the expected decoder id and protocol version, and it learns the
//! decoder version from the first HELLO; every later worker of the pool must report the same version
//! (`worker_version_mismatch` otherwise). Until the first HELLO the version reads `unknown`.
//!
//! Declared sizes come from the container (hostile input): their byte arithmetic is checked and an
//! overflow is `LimitExceeded av1-declared-dimensions` before any worker starts (test
//! `hostile_declared_dims_u32_max_are_refused_before_any_spawn`).
//!
//! NOT_PROVEN: an operating-system memory/CPU cap for the worker (Windows Job Object) needs `unsafe`
//! FFI, which this crate forbids; the declared-dimension decoder limit, the output cap and the
//! wall-clock kill are the enforced bounds.
#![deny(clippy::arithmetic_side_effects)]
#[cfg(feature = "test-faults")]
use crate::test_fault::WorkerTestFault;
use crate::wire::{self, OpenRequest, Reply, WireError, WireFrame};
use hsk_studio_accord::CancellationToken;
use hsk_studio_reel::{
    Av1Config, ChunkSink, CodecDescriptor, DecodedPicture, DecoderFault, DecoderIdentity,
    ExternalVideoDecoder, ISOBMFF_PROVIDER_ID, IsobmffProvider, Limits, MATROSKA_PROVIDER_ID,
    MatroskaProvider, MediaProvider, OutputMode, PictureSink, ReadSeek, ReelError, SegmentReceipt,
    SegmentRequest, StreamLayout, VideoCodec, VideoDecodeError, VideoDecodeSession, VideoRoute,
    VideoStreamSpec, av1_descriptor, detect_type, parse_av1c,
};
use std::{
    collections::VecDeque,
    io::SeekFrom,
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
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
/// Cap for a wait the platform clock cannot represent (`Instant` overflow).
const LONGEST_WAIT: Duration = Duration::from_secs(86_400);
/// Largest packet a request frame can carry (frame cap minus header room).
const MAX_PACKET_BYTES: usize = wire::MAX_REQUEST_FRAME - 64;
/// Decoder the `reel-native-av1-worker` of this package hosts (reel's rav1d decoder, reel
/// `av1::DECODER_ID`). The host checks it in HELLO; it does not link the decoder.
const WORKER_DECODER_ID: &str = "rav1d";
/// Decoder version reported before any worker of the pool completed HELLO.
const DECODER_VERSION_UNKNOWN: &str = "unknown";
/// Bounds for a decoder version string a worker may report, and for the distinct versions interned.
const MAX_VERSION_BYTES: usize = 64;
const MAX_INTERNED_VERSIONS: usize = 8;

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
    /// Fault the spawned workers inject (`test-faults` feature only; product builds have no field).
    #[cfg(feature = "test-faults")]
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
            #[cfg(feature = "test-faults")]
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

    /// The reel-vocabulary form used by the `MediaProvider` port: process and protocol faults are reel's
    /// `ReelError::DecoderFault` (`Crashed`, `Deadline`, `Protocol`); the other contained faults are
    /// resource or availability statements (see `Av1WorkerFault::into_decode_error`).
    pub fn into_reel(self) -> ReelError {
        match self {
            Self::Reel(error) => error,
            Self::Worker(fault) => fault.into_decode_error().into_reel(),
        }
    }
}

impl Av1WorkerFault {
    /// Reel's error vocabulary for a contained worker fault: a crashed, hung or protocol-breaking worker
    /// is a `DecoderFault` (says nothing about the media); an oversize reply, respawn bound or busy pool
    /// is a limit; a version mismatch is unsupported; a worker that cannot start is an unavailable codec.
    fn into_decode_error(self) -> VideoDecodeError {
        match self {
            Self::DecoderCrashed { .. } => DecoderFault::Crashed.into(),
            Self::DecoderDeadline => DecoderFault::Deadline.into(),
            Self::Protocol => DecoderFault::Protocol.into(),
            Self::VersionMismatch => ReelError::Unsupported { what: self.code() }.into(),
            Self::SpawnFailed => ReelError::CodecUnavailable {
                codec: AV1_PROVIDER_ID.to_owned(),
            }
            .into(),
            Self::OutputTooLarge | Self::RespawnLimited | Self::PoolBusy => {
                ReelError::LimitExceeded { what: self.code() }.into()
            }
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

/// `now + wait` without overflow: a wait the clock cannot represent is capped at `LONGEST_WAIT`.
fn deadline_after(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait)
        .or_else(|| now.checked_add(LONGEST_WAIT))
        .unwrap_or(now)
}

/// A decoder version a worker reported, as a `&'static str` for receipts. Only short printable ASCII
/// is accepted, and at most `MAX_INTERNED_VERSIONS` distinct strings are ever kept per process (each
/// kept once), so a worker cannot grow host memory through HELLO.
fn intern_version(version: &str) -> Option<&'static str> {
    static VERSIONS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    if version.is_empty()
        || version.len() > MAX_VERSION_BYTES
        || !version.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return None;
    }
    let mut known = lock(&VERSIONS);
    if let Some(found) = known.iter().find(|known| **known == version) {
        return Some(found);
    }
    if known.len() >= MAX_INTERNED_VERSIONS {
        return None;
    }
    let kept: &'static str = Box::leak(version.to_owned().into_boxed_str());
    known.push(kept);
    Some(kept)
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
        let until = deadline_after(patience);
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
    #[cfg(feature = "test-faults")]
    fault_env: Option<&'static str>,
    /// Decoder version learned from the first HELLO of this pool.
    decoder_version: OnceLock<&'static str>,
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

    fn identity(&self) -> DecoderIdentity {
        DecoderIdentity {
            id: WORKER_DECODER_ID,
            version: self
                .decoder_version
                .get()
                .copied()
                .unwrap_or(DECODER_VERSION_UNKNOWN),
        }
    }

    /// Accepts a HELLO naming this protocol version and the expected decoder; the first accepted
    /// version is pinned for the pool and every later worker must report the same one.
    fn accept_hello(&self, hello: &wire::Hello) -> bool {
        if hello.protocol != wire::PROTOCOL_VERSION || hello.decoder_id != WORKER_DECODER_ID {
            return false;
        }
        intern_version(&hello.decoder_version)
            .is_some_and(|version| *self.decoder_version.get_or_init(|| version) == version)
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
        #[cfg(feature = "test-faults")]
        {
            if let Some(value) = self.fault_env {
                command.env(WorkerTestFault::ENV, value);
            }
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
                Ok(hello) if self.accept_hello(&hello) => Ok(session),
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
        let wall = deadline_after(self.batch_deadline).min(clock.segment_wall);
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
                    frames = frames.saturating_add(1);
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

/// Worker fault behind the error the route saw, for the typed `decode_av1` result (reel's error
/// vocabulary is closed, so the exit code of a crash would otherwise be lost on the way up).
type FaultSlot = Arc<Mutex<Option<Av1WorkerFault>>>;

fn to_picture(frame: WireFrame) -> DecodedPicture {
    DecodedPicture {
        tag: frame.tag,
        format: frame.format,
        film_grain_applied: frame.film_grain_applied,
        planes: frame.planes,
    }
}

/// One open decode on a pooled worker, driven by reel's packet pump as a `VideoDecodeSession`. Returns the
/// worker to the idle pool when dropped, unless a failure made it unusable.
struct WorkerSession {
    pool: Arc<Pool>,
    session: Option<Session>,
    segment_wall: Instant,
    reply_cap: usize,
    max_packet_bytes: usize,
    faults: Option<FaultSlot>,
}

impl Drop for WorkerSession {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            self.pool.checkin(session);
        }
    }
}

impl WorkerSession {
    fn convert(faults: Option<&FaultSlot>, error: Av1Error) -> VideoDecodeError {
        match error {
            Av1Error::Reel(error) => error.into(),
            Av1Error::Worker(fault) => {
                if let Some(slot) = faults {
                    *lock(slot) = Some(fault);
                }
                fault.into_decode_error()
            }
        }
    }

    fn request(
        &mut self,
        frame: Vec<u8>,
        id: u64,
        want: Want,
        cancel: &CancellationToken,
        on_frame: &mut dyn FnMut(WireFrame) -> Result<(), ReelError>,
    ) -> Result<(), VideoDecodeError> {
        let Some(session) = self.session.as_mut().filter(|session| session.usable) else {
            return Err(ReelError::Unsupported {
                what: "av1-worker-session-ended",
            }
            .into());
        };
        let call = Call {
            id,
            frame,
            cap: self.reply_cap,
            want,
        };
        let clock = Clock {
            cancel,
            segment_wall: self.segment_wall,
        };
        self.pool
            .exchange(session, call, &clock, on_frame)
            .map_err(|error| Self::convert(self.faults.as_ref(), error))
    }
}

impl VideoDecodeSession for WorkerSession {
    fn send(
        &mut self,
        packet: &[u8],
        tag: u64,
        cancel: &CancellationToken,
        out: &mut dyn PictureSink,
    ) -> Result<(), VideoDecodeError> {
        if packet.len() > self.max_packet_bytes {
            return Err(ReelError::LimitExceeded {
                what: "av1-packet-bytes",
            }
            .into());
        }
        if packet.is_empty() {
            return Ok(()); // an empty unit decodes to nothing, as in-process
        }
        let id = self.pool.next_id();
        self.request(
            wire::encode_packet(id, tag, packet),
            id,
            Want::BatchEnd,
            cancel,
            &mut |frame| out.accept(to_picture(frame)),
        )
    }

    fn finish(
        &mut self,
        cancel: &CancellationToken,
        out: &mut dyn PictureSink,
    ) -> Result<(), VideoDecodeError> {
        let id = self.pool.next_id();
        self.request(
            wire::encode_finish(id),
            id,
            Want::BatchEnd,
            cancel,
            &mut |frame| out.accept(to_picture(frame)),
        )
    }
}

/// `decode_av1`'s view of the provider: the same decoder, plus a slot that keeps the worker fault.
struct Tracked<'a> {
    provider: &'a WorkerAv1Provider,
    faults: FaultSlot,
}

impl ExternalVideoDecoder for Tracked<'_> {
    fn identity(&self) -> DecoderIdentity {
        self.provider.identity()
    }

    fn supports(&self, codec: VideoCodec) -> bool {
        self.provider.supports(codec)
    }

    fn open(
        &self,
        spec: &VideoStreamSpec<'_>,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn VideoDecodeSession>, VideoDecodeError> {
        let session = self
            .provider
            .open_session(spec, cancel, Some(Arc::clone(&self.faults)))?;
        Ok(Box::new(session))
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
                #[cfg(feature = "test-faults")]
                fault_env: options.test_fault.env_value(),
                decoder_version: OnceLock::new(),
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
    /// contained worker faults distinct from media errors. The container is demuxed by reel's providers
    /// on an UNTRUSTED route whose only decoder is this provider, so nothing here can reach the
    /// in-process decoder. Pictures already handed to `sink` before an error stay delivered: an `Err`
    /// means the segment is incomplete.
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
        verify_grant(src, req)?;
        let head = read_head(src)?;
        let faults = FaultSlot::default();
        let tracked = Tracked {
            provider: self,
            faults: Arc::clone(&faults),
        };
        let route = VideoRoute::untrusted().with_external(&tracked);
        let mut routed = req.clone();
        let result = if IsobmffProvider.sniff(&head) {
            routed.codec = ISOBMFF_PROVIDER_ID.to_owned();
            IsobmffProvider.decode_segment_routed(src, &routed, limits, cancel, sink, &route)
        } else if MatroskaProvider.sniff(&head) {
            routed.codec = MATROSKA_PROVIDER_ID.to_owned();
            MatroskaProvider.decode_segment_routed(src, &routed, limits, cancel, sink, &route)
        } else {
            return Err(ReelError::ImportUnsupported {
                detected: detect_type(&head).to_owned(),
            }
            .into());
        };
        result.map_err(|error| match lock(&faults).take() {
            Some(fault) => Av1Error::Worker(fault),
            None => Av1Error::Reel(error),
        })
    }

    /// Checks a worker out of the pool, opens a decoder on it for the stream and returns the session.
    fn open_session(
        &self,
        spec: &VideoStreamSpec<'_>,
        cancel: &CancellationToken,
        faults: Option<FaultSlot>,
    ) -> Result<WorkerSession, VideoDecodeError> {
        check_cancel(cancel)?;
        let pool = &self.pool;
        // The route validated the record already; the declared size caps below need its fields. Both
        // caps come from container-declared (hostile) sizes and are checked before any worker starts.
        let parsed = if spec.config.is_empty() {
            None
        } else {
            Some(parse_av1c(spec.config)?)
        };
        let frame_cap =
            declared_frame_cap(parsed.as_ref(), spec.width, spec.height, &spec.limits)?;
        let open = OpenRequest {
            max_chunk_bytes: u64::try_from(decoder_budget(spec.width, spec.height, &spec.limits)?)
                .unwrap_or(u64::MAX),
            color: spec.color,
            config: spec.config.to_vec(),
        };
        let mut worker = WorkerSession {
            pool: Arc::clone(pool),
            session: None,
            segment_wall: deadline_after(pool.segment_deadline),
            reply_cap: frame_cap.saturating_add(wire::REPLY_SLACK),
            max_packet_bytes: spec.limits.max_chunk_bytes.min(MAX_PACKET_BYTES),
            faults,
        };
        match pool.checkout() {
            Ok(session) => worker.session = Some(session),
            Err(fault) => {
                return Err(WorkerSession::convert(
                    worker.faults.as_ref(),
                    Av1Error::Worker(fault),
                ));
            }
        }
        let id = pool.next_id();
        // OPEN is answered by OPENED alone: a picture here is the worker breaking the protocol.
        worker.request(
            wire::encode_open(id, &open),
            id,
            Want::Opened,
            cancel,
            &mut |_| {
                Err(ReelError::DecoderFault {
                    fault: DecoderFault::Protocol,
                })
            },
        )?;
        Ok(worker)
    }
}

impl ExternalVideoDecoder for WorkerAv1Provider {
    /// Decoder id checked in HELLO; version learned from the first worker HELLO (`unknown` before).
    fn identity(&self) -> DecoderIdentity {
        self.pool.identity()
    }

    fn supports(&self, codec: VideoCodec) -> bool {
        codec == VideoCodec::Av1
    }

    fn open(
        &self,
        spec: &VideoStreamSpec<'_>,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn VideoDecodeSession>, VideoDecodeError> {
        Ok(Box::new(self.open_session(spec, cancel, None)?))
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
        filled = filled.saturating_add(n);
    }
    src.seek(SeekFrom::Start(0))?;
    Ok(head[..filled].to_vec())
}

/// Declared dimensions whose byte size does not fit u64 (hostile container values).
fn declared_too_large() -> ReelError {
    ReelError::LimitExceeded {
        what: "av1-declared-dimensions",
    }
}

/// Largest packed picture the declared dimensions allow (container track size, `av1C` chroma and bit
/// depth; worst case 4:4:4 16-bit when the record is absent), never above the caller's chunk limit.
/// Declared sizes are hostile input: an overflowing size is `LimitExceeded av1-declared-dimensions`.
fn declared_frame_cap(
    config: Option<&Av1Config>,
    width: u32,
    height: u32,
    limits: &Limits,
) -> Result<usize, ReelError> {
    if width == 0 || height == 0 {
        return Ok(limits.max_chunk_bytes);
    }
    let (w, h) = (u64::from(width), u64::from(height));
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let luma = w.checked_mul(h);
    // Two chroma planes of `plane` samples each, on top of the luma plane.
    let with_chroma = |plane: Option<u64>| {
        plane
            .and_then(|plane| plane.checked_mul(2))
            .and_then(|chroma| luma?.checked_add(chroma))
    };
    let samples = match config {
        Some(c) if c.monochrome => luma,
        Some(c) if c.subsampling_x && c.subsampling_y => with_chroma(cw.checked_mul(ch)),
        Some(c) if c.subsampling_x => with_chroma(cw.checked_mul(h)),
        _ => luma.and_then(|luma| luma.checked_mul(3)),
    };
    let bytes_per_sample: u64 = match config {
        Some(c) if !c.high_bitdepth => 1,
        _ => 2,
    };
    let bytes = samples
        .and_then(|samples| samples.checked_mul(bytes_per_sample))
        .ok_or_else(declared_too_large)?;
    Ok(usize::try_from(bytes)
        .unwrap_or(usize::MAX)
        .min(limits.max_chunk_bytes))
}

/// The decoder-side picture bound sent to the worker: rav1d's pixel limit is this divided by six (the
/// worst packed bytes per pixel), so a stream that declares more pixels than its container is refused
/// by the decoder before it allocates the picture. Overflow is `LimitExceeded` as above.
fn decoder_budget(width: u32, height: u32, limits: &Limits) -> Result<usize, ReelError> {
    if width == 0 || height == 0 {
        return Ok(limits.max_chunk_bytes);
    }
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(6))
        .ok_or_else(declared_too_large)?;
    Ok(usize::try_from(bytes)
        .unwrap_or(usize::MAX)
        .min(limits.max_chunk_bytes))
}

impl MediaProvider for WorkerAv1Provider {
    /// Reel's AV1 capability descriptor with the decoder version learned from HELLO (`unknown` before
    /// the first worker started).
    fn descriptor(&self) -> CodecDescriptor {
        av1_descriptor(self.pool.identity().version, true)
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
