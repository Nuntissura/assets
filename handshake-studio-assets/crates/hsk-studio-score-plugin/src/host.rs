//! Host side: `IsolatedHost`, a [`PluginProvider`] that runs every plugin call in a short-lived or
//! per-instance `score-plugin-worker` child process this crate spawns itself. The host process never
//! loads, maps or probes foreign plugin code, and there is no in-process fallback.
//!
//! Supervision (one request = one write; the reply is read by a per-worker I/O thread, the calling
//! thread only waits on a channel in 5 ms slices), in order of precedence:
//! * cancel: the worker is killed and reaped, the call returns `Canceled`, nothing is published;
//! * deadline: the worker is killed and reaped, `Timeout` (`ProbeTimeout` during spawn, probe and
//!   activation);
//! * crash: end of stream (exit, abort, external kill) is `Crashed{exit_code}` and the worker is reaped;
//! * protocol: garbage, version skew, an oversize or truncated frame, an unexpected kind or a reply
//!   for another request kills the worker, `Protocol(..)`.
//!
//! After any of those the output block holds silence (never partial plugin data) and the instance is
//! `Draining`: only `retire` is legal. Only `Child` handles this host spawned are ever killed. Workers
//! get an empty environment, a null stderr, piped stdin/stdout and, on Windows, `CREATE_NO_WINDOW`, so
//! they never show a console window or take foreground focus.
//!
//! Admission (bounded, never waits): `max_workers` live workers and `max_in_flight_bytes` of reserved
//! block and state buffers; both permits are held until the worker is provably reaped. Every spawned
//! process is registered in a host-owned ledger (owner session, start time, pid, kill/reap state).
//!
//! NOT_PROVEN: an operating-system memory or CPU cap for the worker (a Windows Job Object needs
//! `unsafe` FFI, which this crate forbids); the wall-clock kill and the frame and block bounds are the
//! enforced limits.
use crate::error::{PluginError, ProtocolFault};
use crate::types::{
    ApprovedPlugin, Arch, Completeness, FaultReport, LifecycleState, MAX_DEADLINE, Parameter,
    PluginDescriptor, PluginFormat, PluginGrant, PluginInstance, PluginProvider, ProcessConfig,
    ProcessReceipt, RetireReport, SUPPORTED_CLAP_ABI, StateCapture,
};
use crate::wire::{self, ConfigureSpec, Frame, PluginSpec, Reply, WireError};
use crate::worker::{BACKEND_UNAVAILABLE_EXIT_CODE, WORKER_PROGRAM};
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::{ChannelLayout, InputBlock, OutputBlock, sha256};
use std::{
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

/// How often a waiting host thread re-checks cancel and the wall-clock deadline.
const WAIT_SLICE: Duration = Duration::from_millis(5);
/// How long a worker that closed its stdout gets to exit by itself (to read its true exit code).
const EXIT_PATIENCE: Duration = Duration::from_millis(500);
/// In-flight bytes admitted for a probe (no audio buffers).
const PROBE_BYTES: u64 = 64 << 10;

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

/// Host-wide bounds. Zero values are rejected by [`IsolatedHost::new`].
#[derive(Clone, Debug)]
pub struct HostLimits {
    /// Most worker processes alive at once (probes included); a call beyond that is refused at once.
    pub max_workers: usize,
    /// Most bytes of block and state buffers reserved across all live workers.
    pub max_in_flight_bytes: u64,
    /// Largest frame either side accepts; bounds `max_block_frames * channels * 4`.
    pub max_frame_bytes: u32,
    /// Largest approved plugin file the host will hash.
    pub max_plugin_file_bytes: u64,
    /// Time a new worker gets to complete its HELLO handshake.
    pub spawn_timeout: Duration,
    /// Time retire waits for the worker to exit by itself before killing it.
    pub shutdown_grace: Duration,
}

impl Default for HostLimits {
    fn default() -> Self {
        Self {
            max_workers: 2,
            max_in_flight_bytes: 256 << 20,
            max_frame_bytes: 70 << 20,
            max_plugin_file_bytes: 512 << 20,
            spawn_timeout: Duration::from_secs(10),
            shutdown_grace: Duration::from_millis(500),
        }
    }
}

/// Which backend the worker process runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkerBackend {
    /// Pure-Rust gain-and-delay reference (never loads the plugin file). Proves isolation only.
    #[default]
    Reference,
    /// Loads the approved `.clap` library in the worker. The worker binary must be built with cargo
    /// feature `clap`; otherwise it exits with `BACKEND_UNAVAILABLE_EXIT_CODE` and the host reports
    /// `unavailable`.
    Clap,
}

/// Host construction options.
#[derive(Clone, Debug, Default)]
pub struct HostOptions {
    /// Worker executable. `None`: `score-plugin-worker` next to the running executable (or one
    /// directory above it, for test and example binaries).
    pub program: Option<PathBuf>,
    pub limits: HostLimits,
    pub backend: WorkerBackend,
    /// Command-line arguments for the worker (test fault injection and the reference latency).
    #[doc(hidden)]
    pub test_args: Vec<String>,
}

impl HostOptions {
    pub fn with_worker_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = Some(program.into());
        self
    }
}

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
pub struct HostStats {
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

struct Shared {
    program: PathBuf,
    limits: HostLimits,
    test_args: Vec<String>,
    workers: AtomicUsize,
    bytes: AtomicU64,
    ledger: Mutex<Vec<ProcessLedgerEntry>>,
    counters: Counters,
    next_instance: AtomicU64,
}

/// A held share of the worker and byte budget; released on drop, which for a session is after the reap.
struct Permit {
    shared: Arc<Shared>,
    bytes: u64,
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.shared.workers.fetch_sub(1, Ordering::AcqRel);
        self.shared.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl Shared {
    /// Reserves one worker and `bytes` or refuses at once; a partial acquisition is released.
    fn admit(self: &Arc<Self>, bytes: u64) -> Result<Permit, PluginError> {
        let max_workers = self.limits.max_workers;
        self.workers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |w| {
                (w < max_workers).then_some(w + 1)
            })
            .map_err(|_| PluginError::BudgetExceeded)?;
        let max_bytes = self.limits.max_in_flight_bytes;
        let reserved = self
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |b| {
                b.checked_add(bytes).filter(|total| *total <= max_bytes)
            });
        if reserved.is_err() {
            self.workers.fetch_sub(1, Ordering::AcqRel);
            return Err(PluginError::BudgetExceeded);
        }
        Ok(Permit {
            shared: Arc::clone(self),
            bytes,
        })
    }
}

type Frames = Result<Frame, WireError>;

struct Job {
    frame: Vec<u8>,
    cap: u32,
}

enum Waited {
    Frame(Frame),
    Canceled,
    TimedOut,
    Failed(WireError),
}

/// Why a call ended without a usable reply.
enum Failure {
    Canceled,
    Deadline,
    Wire(WireError),
    /// A well-framed reply that is the wrong kind or answers another request.
    Mismatch,
}

#[derive(Clone, Copy)]
struct Terminated {
    killed: bool,
    reaped: bool,
    exit_code: Option<i32>,
}

/// One live worker process plus the I/O thread that owns its pipes.
struct Session {
    child: Child,
    jobs: mpsc::Sender<Job>,
    replies: mpsc::Receiver<Frames>,
    /// False once the process was terminated or its stream is out of sync: never reused.
    usable: bool,
    ledger_index: usize,
    pid: u32,
    shared: Arc<Shared>,
    finished: Option<Terminated>,
    /// Backend identity the worker announced in HELLO.
    identity: Arc<str>,
    /// Last field: released only after `Drop::drop` has killed and reaped the child.
    _permit: Permit,
}

impl Session {
    fn wait(&self, cancel: &CancellationToken, wall: Instant) -> Waited {
        loop {
            match self.replies.recv_timeout(WAIT_SLICE) {
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

    /// Reaps the child, killing it if it is still running after `patience`. Idempotent.
    fn terminate(&mut self, patience: Duration) -> Terminated {
        self.usable = false;
        if let Some(done) = self.finished {
            return done;
        }
        let until = Instant::now() + patience;
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

    /// Writes one request and waits for its reply (cancel and the wall-clock deadline polled).
    fn exchange(
        &mut self,
        id: u64,
        frame: Vec<u8>,
        cap: u32,
        deadline: Duration,
        cancel: &CancellationToken,
    ) -> Result<Reply, Failure> {
        if cancel.check().is_err() {
            return Err(Failure::Canceled);
        }
        if self.jobs.send(Job { frame, cap }).is_err() {
            return Err(Failure::Wire(WireError::Closed));
        }
        let received = match self.wait(cancel, Instant::now() + deadline) {
            Waited::Frame(frame) => frame,
            Waited::Canceled => return Err(Failure::Canceled),
            Waited::TimedOut => return Err(Failure::Deadline),
            Waited::Failed(error) => return Err(Failure::Wire(error)),
        };
        let reply = wire::decode_reply(&received).map_err(Failure::Wire)?;
        if reply.id() == id {
            Ok(reply)
        } else {
            Err(Failure::Mismatch)
        }
    }

    /// Terminates the worker for `failure` and returns the typed error plus what happened to it.
    fn fail(&mut self, failure: Failure, probe: bool) -> (PluginError, Terminated) {
        let counters = &self.shared.counters;
        match failure {
            Failure::Canceled => {
                bump(&counters.canceled);
                (PluginError::Canceled, self.terminate(Duration::ZERO))
            }
            Failure::Deadline => {
                bump(&counters.timed_out);
                let error = if probe {
                    PluginError::ProbeTimeout
                } else {
                    PluginError::Timeout
                };
                (error, self.terminate(Duration::ZERO))
            }
            Failure::Wire(WireError::Closed) => {
                bump(&counters.crashed);
                let done = self.terminate(EXIT_PATIENCE);
                (
                    PluginError::Crashed {
                        exit_code: done.exit_code,
                    },
                    done,
                )
            }
            Failure::Wire(error) => {
                bump(&counters.protocol_faults);
                let fault = error.fault().unwrap_or(ProtocolFault::Inconsistent);
                (PluginError::Protocol(fault), self.terminate(Duration::ZERO))
            }
            Failure::Mismatch => {
                bump(&counters.protocol_faults);
                (
                    PluginError::Protocol(ProtocolFault::UnexpectedKind),
                    self.terminate(Duration::ZERO),
                )
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.terminate(Duration::ZERO);
    }
}

/// Owns the worker's pipes: HELLO first, then per job one write and one reply frame.
fn io_loop(
    mut stdin: ChildStdin,
    mut stdout: ChildStdout,
    jobs: &mpsc::Receiver<Job>,
    out: &mpsc::Sender<Frames>,
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
        let reply = wire::read_frame(&mut stdout, job.cap);
        let failed = reply.is_err();
        if out.send(reply).is_err() || failed {
            return;
        }
    }
}

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

/// Spawns a worker, registers it in the ledger and completes the HELLO handshake. From the moment the
/// child exists, every error path drops the `Session`, which kills and reaps it.
fn spawn_session(
    shared: &Arc<Shared>,
    permit: Permit,
    owner_session: &str,
    expected_clap_abi: u32,
    cancel: &CancellationToken,
) -> Result<Session, PluginError> {
    if cancel.check().is_err() {
        return Err(PluginError::Canceled);
    }
    let mut command = Command::new(&shared.program);
    command
        .args(&shared.test_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear();
    quiet(&mut command);
    let mut child = command.spawn().map_err(|_| PluginError::Unavailable)?;
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
        ledger.len() - 1
    };
    let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();
    let (replies_tx, replies_rx) = mpsc::channel::<Frames>();
    let (stdin, stdout) = (child.stdin.take(), child.stdout.take());
    let mut session = Session {
        child,
        jobs: jobs_tx,
        replies: replies_rx,
        usable: true,
        ledger_index,
        pid,
        shared: Arc::clone(shared),
        finished: None,
        identity: Arc::from(""),
        _permit: permit,
    };
    let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
        return Err(PluginError::Unavailable);
    };
    thread::Builder::new()
        .name("score-plugin-worker-io".to_owned())
        .spawn(move || io_loop(stdin, stdout, &jobs_rx, &replies_tx))
        .map_err(|_| PluginError::Unavailable)?;
    let wall = Instant::now() + shared.limits.spawn_timeout;
    let frame = match session.wait(cancel, wall) {
        Waited::Frame(frame) => frame,
        Waited::Canceled => return Err(session.fail(Failure::Canceled, true).0),
        Waited::TimedOut => return Err(session.fail(Failure::Deadline, true).0),
        Waited::Failed(WireError::Closed) => {
            // A worker that exits at once with the "backend not built" code never said HELLO.
            let (error, done) = session.fail(Failure::Wire(WireError::Closed), true);
            return Err(if done.exit_code == Some(BACKEND_UNAVAILABLE_EXIT_CODE) {
                PluginError::Unavailable
            } else {
                error
            });
        }
        Waited::Failed(error) => return Err(session.fail(Failure::Wire(error), true).0),
    };
    let hello = match wire::decode_hello(&frame) {
        Ok(hello) => hello,
        Err(error) => return Err(session.fail(Failure::Wire(error), true).0),
    };
    if hello.clap_abi != expected_clap_abi {
        session.terminate(Duration::ZERO);
        return Err(PluginError::AbiMismatch);
    }
    session.identity = Arc::from(hello.identity);
    Ok(session)
}

fn check_deadline(deadline: Duration) -> Result<(), PluginError> {
    if deadline.is_zero() || deadline > MAX_DEADLINE {
        Err(PluginError::InvalidConfig)
    } else {
        Ok(())
    }
}

/// Grant, architecture and ABI gate; returns the grant's owner session for the ledger. Runs before
/// any process exists and before the file is read.
fn authorize(plugin: &ApprovedPlugin, grant: Option<&PluginGrant>) -> Result<String, PluginError> {
    let grant = grant.ok_or(PluginError::Unauthorized)?;
    if grant.content_sha256 != plugin.content_sha256 {
        return Err(PluginError::Unauthorized);
    }
    if Arch::host() != Some(plugin.arch)
        || plugin.format != PluginFormat::Clap
        || plugin.abi_version != SUPPORTED_CLAP_ABI
    {
        return Err(PluginError::AbiMismatch);
    }
    Ok(grant.actor.session_id().to_owned())
}

/// Reads the approved file (bounded) and compares its SHA-256 with the approval. This is a read, not a
/// load: the host never maps or executes the file.
fn verify_content(plugin: &ApprovedPlugin, max_bytes: u64) -> Result<(), PluginError> {
    let meta = std::fs::metadata(&plugin.path).map_err(|_| PluginError::Unavailable)?;
    if !meta.is_file() {
        return Err(PluginError::Unavailable);
    }
    if meta.len() > max_bytes {
        return Err(PluginError::BudgetExceeded);
    }
    let bytes = std::fs::read(&plugin.path).map_err(|_| PluginError::Unavailable)?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > max_bytes) {
        return Err(PluginError::BudgetExceeded);
    }
    if sha256(&bytes) == plugin.content_sha256 {
        Ok(())
    } else {
        Err(PluginError::HashMismatch)
    }
}

fn spec_of(plugin: &ApprovedPlugin) -> Result<PluginSpec, PluginError> {
    let path = plugin
        .path
        .to_str()
        .filter(|p| !p.is_empty() && p.len() <= 4096)
        .ok_or(PluginError::InvalidConfig)?;
    Ok(PluginSpec {
        format: plugin.format.wire(),
        arch: plugin.arch.wire(),
        abi: plugin.abi_version,
        sha: plugin.content_sha256,
        path: path.to_owned(),
    })
}

/// Process-isolated plugin provider. Cheap to clone: clones share one budget, ledger and counters.
#[derive(Clone)]
pub struct IsolatedHost {
    shared: Arc<Shared>,
}

impl IsolatedHost {
    pub fn new(options: HostOptions) -> Result<Self, PluginError> {
        let limits = &options.limits;
        if limits.max_workers == 0
            || limits.max_in_flight_bytes == 0
            || limits.max_frame_bytes == 0
            || limits.max_plugin_file_bytes == 0
            || limits.spawn_timeout.is_zero()
            || limits.shutdown_grace.is_zero()
        {
            return Err(PluginError::InvalidConfig);
        }
        let program = options
            .program
            .clone()
            .or_else(discover_program)
            .filter(|path| path.is_file())
            .ok_or(PluginError::Unavailable)?;
        Ok(Self {
            shared: Arc::new(Shared {
                program,
                limits: options.limits,
                test_args: {
                    let mut args = Vec::new();
                    if options.backend == WorkerBackend::Clap {
                        args.extend(["--backend".to_owned(), "clap".to_owned()]);
                    }
                    args.extend(options.test_args);
                    args
                },
                workers: AtomicUsize::new(0),
                bytes: AtomicU64::new(0),
                ledger: Mutex::new(Vec::new()),
                counters: Counters::default(),
                next_instance: AtomicU64::new(1),
            }),
        })
    }

    pub fn stats(&self) -> HostStats {
        let c = &self.shared.counters;
        HostStats {
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

    /// Snapshot of every process this host ever spawned.
    pub fn ledger(&self) -> Vec<ProcessLedgerEntry> {
        lock(&self.shared.ledger).clone()
    }

    /// `activate` with the concrete instance type (exposes the worker pid for ledger checks).
    pub fn activate_isolated(
        &self,
        config: &ProcessConfig,
        cancel: &CancellationToken,
        deadline: Duration,
    ) -> Result<IsolatedInstance, PluginError> {
        cancel.check().map_err(|_| PluginError::Canceled)?;
        check_deadline(deadline)?;
        let owner = authorize(&config.plugin, config.grant.as_ref())?;
        config.validate()?;
        let channels = config.layout.channels();
        let block_bytes = u64::try_from(channels * config.max_block_frames * 4)
            .map_err(|_| PluginError::BudgetExceeded)?;
        let reply_cap = u32::try_from(block_bytes + u64::from(wire::REPLY_SLACK))
            .map_err(|_| PluginError::BudgetExceeded)?;
        let limits = &self.shared.limits;
        if reply_cap > limits.max_frame_bytes
            || config.max_state_bytes.saturating_add(wire::REPLY_SLACK) > limits.max_frame_bytes
        {
            return Err(PluginError::BudgetExceeded);
        }
        // Request and reply buffers plus the largest state blob stay reserved until the reap.
        let reserved = block_bytes * 2 + u64::from(config.max_state_bytes);
        let permit = self.shared.admit(reserved)?;
        verify_content(&config.plugin, limits.max_plugin_file_bytes)?;
        let spec = spec_of(&config.plugin)?;
        let mut session = spawn_session(
            &self.shared,
            permit,
            &owner,
            config.plugin.abi_version,
            cancel,
        )?;
        let cfg = ConfigureSpec {
            sample_rate_hz: config.sample_rate_hz,
            channels: u16::try_from(channels).map_err(|_| PluginError::InvalidConfig)?,
            max_frames: u32::try_from(config.max_block_frames)
                .map_err(|_| PluginError::InvalidConfig)?,
            params: config.parameters.iter().map(|p| (p.id, p.value)).collect(),
        };
        let request_id = 1;
        let frame = wire::encode_configure(request_id, &spec, &cfg);
        let reply = session.exchange(request_id, frame, wire::REPLY_SLACK, deadline, cancel);
        let (identity, latency, version) = match reply {
            Ok(Reply::Configured {
                identity,
                latency,
                version,
                ..
            }) => (identity, latency, version),
            Ok(Reply::Error { code, .. }) => {
                session.terminate(Duration::ZERO);
                return Err(wire::error_of(code));
            }
            Ok(_) => return Err(session.fail(Failure::Mismatch, true).0),
            Err(failure) => return Err(session.fail(failure, true).0),
        };
        let state = LifecycleState::Discovered
            .transition(LifecycleState::Probed)
            .and_then(|s| s.transition(LifecycleState::Active))?;
        let pid = Some(session.pid);
        Ok(IsolatedInstance {
            session: Some(session),
            state,
            id: self.shared.next_instance.fetch_add(1, Ordering::AcqRel),
            identity: Arc::from(identity),
            layout: config.layout.clone(),
            sample_rate_hz: config.sample_rate_hz,
            max_block_frames: config.max_block_frames,
            max_state_bytes: config.max_state_bytes,
            latency_samples: latency,
            state_version: version,
            blocks: 0,
            next_request: request_id + 1,
            fault: None,
            retired: None,
            reserved_bytes: reserved,
            pid,
        })
    }
}

impl PluginProvider for IsolatedHost {
    fn probe(
        &self,
        plugin: &ApprovedPlugin,
        grant: Option<&PluginGrant>,
        cancel: &CancellationToken,
        deadline: Duration,
    ) -> Result<PluginDescriptor, PluginError> {
        cancel.check().map_err(|_| PluginError::Canceled)?;
        check_deadline(deadline)?;
        let owner = authorize(plugin, grant)?;
        let limits = &self.shared.limits;
        let permit = self.shared.admit(PROBE_BYTES)?;
        verify_content(plugin, limits.max_plugin_file_bytes)?;
        let spec = spec_of(plugin)?;
        let mut session = spawn_session(&self.shared, permit, &owner, plugin.abi_version, cancel)?;
        let frame = wire::encode_probe(1, &spec);
        let identity = Arc::clone(&session.identity);
        let result = match session.exchange(1, frame, wire::REPLY_SLACK, deadline, cancel) {
            Ok(Reply::Probed {
                plugin_id,
                name,
                format,
                abi,
                inputs,
                outputs,
                ..
            }) => PluginFormat::from_wire(format)
                .map(|format| PluginDescriptor {
                    plugin_id,
                    name,
                    format,
                    abi_version: abi,
                    inputs,
                    outputs,
                    provider_identity: identity.to_string(),
                })
                .ok_or(PluginError::Protocol(ProtocolFault::Inconsistent)),
            Ok(Reply::Error { code, .. }) => Err(wire::error_of(code)),
            Ok(_) => Err(session.fail(Failure::Mismatch, true).0),
            Err(failure) => Err(session.fail(failure, true).0),
        };
        // A probe worker is short-lived: ask it to leave, then reap it either way.
        if session.usable {
            let _ = session.exchange(
                2,
                wire::encode_shutdown(2),
                wire::REPLY_SLACK,
                limits.shutdown_grace,
                cancel,
            );
        }
        session.terminate(limits.shutdown_grace);
        result
    }

    fn activate(
        &self,
        config: &ProcessConfig,
        cancel: &CancellationToken,
        deadline: Duration,
    ) -> Result<Box<dyn PluginInstance>, PluginError> {
        Ok(Box::new(self.activate_isolated(config, cancel, deadline)?))
    }
}

/// One activated plugin in its own worker process.
pub struct IsolatedInstance {
    /// Dropped last-but-one: killing and reaping happens in `Session::drop`, then the permit is released.
    session: Option<Session>,
    state: LifecycleState,
    id: u64,
    identity: Arc<str>,
    layout: ChannelLayout,
    sample_rate_hz: u32,
    max_block_frames: usize,
    max_state_bytes: u32,
    latency_samples: u32,
    state_version: u64,
    blocks: u64,
    next_request: u64,
    fault: Option<FaultReport>,
    retired: Option<RetireReport>,
    reserved_bytes: u64,
    pid: Option<u32>,
}

impl std::fmt::Debug for IsolatedInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IsolatedInstance")
            .field("id", &self.id)
            .field("pid", &self.pid)
            .field("state", &self.state)
            .field("state_version", &self.state_version)
            .finish_non_exhaustive()
    }
}

impl IsolatedInstance {
    /// Operating-system id of the worker process (also in the host ledger).
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn blocks_processed(&self) -> u64 {
        self.blocks
    }

    fn require_active(&self) -> Result<(), PluginError> {
        if self.state == LifecycleState::Active {
            Ok(())
        } else {
            Err(PluginError::IllegalTransition {
                from: self.state,
                to: LifecycleState::Processing,
            })
        }
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_request;
        self.next_request += 1;
        id
    }

    /// Records a killed or crashed worker: the instance can only be retired from here.
    fn fail(&mut self, failure: Failure, silenced: bool) -> PluginError {
        let (error, done) = match self.session.as_mut() {
            Some(session) => session.fail(failure, false),
            None => (
                PluginError::Unavailable,
                Terminated {
                    killed: false,
                    reaped: true,
                    exit_code: None,
                },
            ),
        };
        self.state = LifecycleState::Draining;
        self.fault = Some(FaultReport {
            error,
            killed: done.killed,
            reaped: done.reaped,
            exit_code: done.exit_code,
            safe_output: silenced,
        });
        error
    }

    /// Records a typed refusal from a live worker: the instance stays usable.
    fn refuse(&mut self, error: PluginError, silenced: bool) -> PluginError {
        self.state = LifecycleState::Active;
        self.fault = Some(FaultReport {
            error,
            killed: false,
            reaped: false,
            exit_code: None,
            safe_output: silenced,
        });
        error
    }

    /// Orderly end: Shutdown request, then reap (kill after the grace period), then the permits.
    pub fn retire_in_place(&mut self) -> RetireReport {
        if let Some(report) = &self.retired {
            return report.clone();
        }
        if self.state != LifecycleState::Draining {
            self.state = self
                .state
                .transition(LifecycleState::Draining)
                .unwrap_or(LifecycleState::Draining);
        }
        let mut done = Terminated {
            killed: false,
            reaped: true,
            exit_code: None,
        };
        if let Some(mut session) = self.session.take() {
            let grace = session.shared.limits.shutdown_grace;
            if session.usable {
                let id = self.next_request;
                let _ = session.exchange(
                    id,
                    wire::encode_shutdown(id),
                    wire::REPLY_SLACK,
                    grace,
                    &CancellationToken::default(),
                );
            }
            done = session.terminate(grace);
            // `session` drops here: the permits are released after the reap.
        }
        self.state = LifecycleState::Retired;
        let report = RetireReport {
            instance_id: self.id,
            pid: self.pid,
            blocks_processed: self.blocks,
            killed: done.killed,
            reaped: done.reaped,
            exit_code: done.exit_code,
            released_bytes: self.reserved_bytes,
        };
        self.retired = Some(report.clone());
        report
    }
}

fn silence(out: &mut OutputBlock, frames: usize) {
    for channel in out.channels_mut() {
        let end = frames.min(channel.len());
        channel[..end].fill(0.0);
    }
}

impl PluginInstance for IsolatedInstance {
    fn lifecycle(&self) -> LifecycleState {
        self.state
    }
    fn instance_id(&self) -> u64 {
        self.id
    }
    fn provider_identity(&self) -> &str {
        &self.identity
    }
    fn latency_samples(&self) -> u32 {
        self.latency_samples
    }
    fn state_version(&self) -> u64 {
        self.state_version
    }
    fn layout(&self) -> &ChannelLayout {
        &self.layout
    }
    fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }
    fn max_block_frames(&self) -> usize {
        self.max_block_frames
    }

    fn process_expecting(
        &mut self,
        expected_state_version: u64,
        input: &InputBlock<'_>,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
        deadline: Duration,
    ) -> Result<ProcessReceipt, PluginError> {
        self.require_active()?;
        check_deadline(deadline)?;
        cancel.check().map_err(|_| PluginError::Canceled)?;
        // Admission: every refusal below leaves `out` untouched and the worker running.
        input.validate().map_err(PluginError::InvalidBlock)?;
        let spec = input.spec();
        if spec.sample_rate_hz != self.sample_rate_hz {
            return Err(PluginError::InvalidBlock(
                hsk_studio_score::ScoreError::InvalidRate,
            ));
        }
        if spec.layout != self.layout || out.layout().channels() != self.layout.channels() {
            return Err(PluginError::InvalidBlock(
                hsk_studio_score::ScoreError::ChannelMismatch,
            ));
        }
        let frames = spec.frames;
        if frames > self.max_block_frames || frames > out.capacity() {
            return Err(PluginError::BudgetExceeded);
        }
        if expected_state_version != self.state_version {
            return Err(PluginError::StaleState);
        }
        let channels = self.layout.channels();
        let mut samples = Vec::with_capacity(channels * frames);
        for channel in input.channels() {
            samples.extend_from_slice(channel);
        }
        let id = self.next_id();
        let frame = wire::encode_process(
            id,
            expected_state_version,
            spec.start_sample,
            u32::try_from(frames).map_err(|_| PluginError::BudgetExceeded)?,
            u16::try_from(channels).map_err(|_| PluginError::InvalidConfig)?,
            &samples,
        );
        let cap = u32::try_from(channels * frames * 4)
            .ok()
            .and_then(|b| b.checked_add(wire::REPLY_SLACK))
            .ok_or(PluginError::BudgetExceeded)?;
        self.state = LifecycleState::Processing;
        let Some(session) = self.session.as_mut() else {
            silence(out, frames);
            return Err(self.fail(Failure::Wire(WireError::Closed), true));
        };
        let reply = session.exchange(id, frame, cap, deadline, cancel);
        match reply {
            Ok(Reply::Processed {
                version,
                completeness,
                latency,
                frames: got_frames,
                channels: got_channels,
                samples: processed,
                ..
            }) => {
                let completeness = Completeness::from_wire(completeness);
                let consistent = version == expected_state_version
                    && usize::try_from(got_frames) == Ok(frames)
                    && usize::from(got_channels) == channels
                    && processed.len() == channels * frames
                    && completeness.is_some()
                    && latency == self.latency_samples;
                let Some(completeness) = completeness.filter(|_| consistent) else {
                    silence(out, frames);
                    return Err(self.fail(Failure::Mismatch, true));
                };
                if processed.iter().any(|s| !s.is_finite()) {
                    silence(out, frames);
                    return Err(self.refuse(PluginError::NonFiniteOutput, true));
                }
                for (ch, dst) in out.channels_mut().enumerate().take(channels) {
                    dst[..frames].copy_from_slice(&processed[ch * frames..(ch + 1) * frames]);
                }
                self.state = LifecycleState::Active;
                self.blocks += 1;
                Ok(ProcessReceipt {
                    instance_id: self.id,
                    frames,
                    start_sample: spec.start_sample,
                    latency_samples: self.latency_samples,
                    state_version: version,
                    provider_identity: Arc::clone(&self.identity),
                    completeness,
                })
            }
            Ok(Reply::Error { code, detail, .. }) => {
                silence(out, frames);
                if code == wire::E_STALE {
                    self.state_version = detail;
                }
                Err(self.refuse(wire::error_of(code), true))
            }
            Ok(_) => {
                silence(out, frames);
                Err(self.fail(Failure::Mismatch, true))
            }
            Err(failure) => {
                silence(out, frames);
                Err(self.fail(failure, true))
            }
        }
    }

    fn set_parameter(
        &mut self,
        parameter: Parameter,
        cancel: &CancellationToken,
        deadline: Duration,
    ) -> Result<u64, PluginError> {
        self.require_active()?;
        check_deadline(deadline)?;
        cancel.check().map_err(|_| PluginError::Canceled)?;
        if !parameter.value.is_finite() {
            return Err(PluginError::InvalidConfig);
        }
        let id = self.next_id();
        let frame = wire::encode_set_param(id, parameter.id, parameter.value);
        let Some(session) = self.session.as_mut() else {
            return Err(self.fail(Failure::Wire(WireError::Closed), false));
        };
        match session.exchange(id, frame, wire::REPLY_SLACK, deadline, cancel) {
            Ok(Reply::ParamSet { version, .. }) => {
                self.state_version = version;
                Ok(version)
            }
            Ok(Reply::Error { code, .. }) => Err(self.refuse(wire::error_of(code), false)),
            Ok(_) => Err(self.fail(Failure::Mismatch, false)),
            Err(failure) => Err(self.fail(failure, false)),
        }
    }

    fn capture_state(
        &mut self,
        cancel: &CancellationToken,
        deadline: Duration,
        max_bytes: u32,
    ) -> Result<StateCapture, PluginError> {
        self.require_active()?;
        check_deadline(deadline)?;
        cancel.check().map_err(|_| PluginError::Canceled)?;
        if max_bytes > self.max_state_bytes {
            return Err(PluginError::BudgetExceeded);
        }
        let id = self.next_id();
        let frame = wire::encode_capture(id, max_bytes);
        let Some(session) = self.session.as_mut() else {
            return Err(self.fail(Failure::Wire(WireError::Closed), false));
        };
        let cap = wire::REPLY_SLACK.saturating_add(max_bytes);
        match session.exchange(id, frame, cap, deadline, cancel) {
            Ok(Reply::State {
                version,
                completeness,
                bytes,
                ..
            }) => {
                let known = Completeness::from_wire(completeness);
                match known {
                    Some(completeness)
                        if bytes.len() <= usize::try_from(max_bytes).unwrap_or(0) =>
                    {
                        self.state_version = version;
                        Ok(StateCapture {
                            state_version: version,
                            completeness,
                            bytes,
                        })
                    }
                    _ => Err(self.fail(Failure::Wire(WireError::TooLarge), false)),
                }
            }
            Ok(Reply::Error { code, .. }) => Err(self.refuse(wire::error_of(code), false)),
            Ok(_) => Err(self.fail(Failure::Mismatch, false)),
            Err(failure) => Err(self.fail(failure, false)),
        }
    }

    fn last_fault(&self) -> Option<&FaultReport> {
        self.fault.as_ref()
    }

    fn retire(mut self: Box<Self>) -> RetireReport {
        self.retire_in_place()
    }
}
