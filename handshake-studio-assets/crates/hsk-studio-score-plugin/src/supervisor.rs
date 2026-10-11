//! Process supervision, in ONE self-contained module with no plugin vocabulary, so it can be moved
//! mechanically to the shared worker-supervision owner once the module map has one (coordinator note:
//! the same supervisor was copied between motion-js and reel-native; do not copy it further).
//!
//! What it owns:
//! * spawn: `Command` with an empty environment (`env_clear`), a null stderr, piped stdin/stdout and, on
//!   Windows, `CREATE_NO_WINDOW`, so a worker is started without a console window (the absence of a window is NOT_PROVEN by any test);
//! * admission: a worker-count and a byte budget acquired without ever waiting ([`Supervisor::admit`]),
//!   held by a [`Permit`] that is released only after the child has been reaped;
//! * ledger: every spawned process (owner session, start time, pid, killed/reaped/exit code) and
//!   monotonic counters ([`SupervisorStats`]);
//! * framed exchange: one request frame written, one reply frame read, by a per-worker I/O thread; the
//!   calling thread only waits on a channel in [`WAIT_SLICE`] slices, polling the cancel token and the
//!   wall-clock deadline;
//! * classification and reclaim: cancel and deadline kill the child at once; end of stream is a crash
//!   (after [`EXIT_PATIENCE`] so the true exit code can be read); a framing failure supplied by the
//!   caller's reader is a protocol fault and kills the child; every path ends with the child killed
//!   or exited AND reaped; dropping a [`Session`] reaps it.
//!
//! What it does not own: the wire format (the caller supplies a `ReadFn` that reads one reply frame and
//! maps its own framing errors into [`ReadFailure`]), the meaning of requests and replies, and error
//! vocabulary (the caller maps [`Classified`] to its own errors). There is no heartbeat: liveness is
//! the request deadline. Only `Child` handles this module spawned are ever killed. NOT_PROVEN: an
//! operating-system memory or CPU cap (a Windows Job Object needs `unsafe`).
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_accord::CancellationToken;
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

/// How often a waiting thread re-checks cancel and the wall-clock deadline.
pub(crate) const WAIT_SLICE: Duration = Duration::from_millis(5);
/// How long a worker that closed its stdout gets to exit by itself (to read its true exit code).
pub(crate) const EXIT_PATIENCE: Duration = Duration::from_millis(500);
/// Longest wait this module will compute an `Instant` for. A caller-supplied duration above it is
/// clamped, so `Instant + Duration` cannot overflow (it panics in std) whatever a limit says
/// (test: `tests::deadline_arithmetic_never_overflows`).
pub(crate) const WAIT_CEILING: Duration = Duration::from_secs(24 * 60 * 60);

/// `now + wait`, clamped to [`WAIT_CEILING`]. If even that cannot be represented the deadline is
/// `now`, so a wait ends at once instead of panicking or never ending.
pub(crate) fn after(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait.min(WAIT_CEILING)).unwrap_or(now)
}

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

/// `program_name` (plus the platform executable suffix) in the running executable's own directory
/// and nowhere else. A parent directory is not searched: a planted binary one level up must not become
/// the worker (callers that run from `deps/` or `examples/` pass the program explicitly).
pub(crate) fn discover_program(program_name: &str) -> Option<PathBuf> {
    find_beside(&std::env::current_exe().ok()?, program_name)
}

fn find_beside(exe: &std::path::Path, program_name: &str) -> Option<PathBuf> {
    let name = format!("{program_name}{}", std::env::consts::EXE_SUFFIX);
    Some(exe.parent()?.join(name)).filter(|candidate| candidate.is_file())
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

/// Supervisor construction parameters (already validated by the caller).
#[derive(Clone, Debug)]
pub(crate) struct SupervisorConfig {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub max_workers: usize,
    pub max_in_flight_bytes: u64,
    /// Time a new worker gets to send its first (HELLO) frame.
    pub spawn_timeout: Duration,
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
pub(crate) struct Refused;

/// A held share of the worker and byte budget; released on drop, which for a session is after the reap.
pub(crate) struct Permit {
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
pub(crate) enum ReadFailure<E> {
    /// End of stream at a frame boundary, or an I/O failure: the peer is gone.
    Closed,
    /// The peer sent something that is not a valid frame.
    Failed(E),
}

/// Reads one reply frame of at most `cap` payload bytes from the worker's stdout.
pub(crate) type ReadFn<F, E> = fn(&mut ChildStdout, u32) -> Result<F, ReadFailure<E>>;

/// Why a call ended without a usable reply frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure<E> {
    Canceled,
    Deadline,
    Read(ReadFailure<E>),
}

/// What [`Session::fail`] decided, for the caller to map into its own error vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Classified<E> {
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
pub(crate) struct Terminated {
    pub killed: bool,
    pub reaped: bool,
    pub exit_code: Option<i32>,
}

/// Why a spawn did not yield a session. The child, if one was started, is already reaped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpawnError<E> {
    /// The cancel token was set before anything was started.
    Canceled,
    /// The program could not be started, or its pipes or I/O thread could not be set up.
    Unavailable,
    /// The worker started but did not deliver a first frame; `Classified` says how it ended.
    Handshake(Classified<E>, Terminated),
}

struct Job {
    frame: Vec<u8>,
    cap: u32,
}

enum Waited<F, E> {
    Frame(F),
    Canceled,
    TimedOut,
    Failed(ReadFailure<E>),
}

/// One live worker process plus the I/O thread that owns its pipes.
pub(crate) struct Session<F, E> {
    child: Child,
    jobs: mpsc::Sender<Job>,
    replies: mpsc::Receiver<Result<F, ReadFailure<E>>>,
    /// False once the process was terminated or its stream is out of sync: never reused.
    usable: bool,
    ledger_index: usize,
    pid: u32,
    shared: Arc<Shared>,
    finished: Option<Terminated>,
    /// Caller state that must outlive the child (for example a private directory the child uses);
    /// dropped after `Drop::drop` has killed and reaped it, before the permit is released.
    _keepalive: Option<Box<dyn Send>>,
    /// Last field: released only after `Drop::drop` has killed and reaped the child.
    _permit: Permit,
}

/// Owns the worker's pipes: the first frame, then per job one write and one reply frame.
fn io_loop<F, E>(
    mut stdin: ChildStdin,
    mut stdout: ChildStdout,
    read: ReadFn<F, E>,
    hello_cap: u32,
    jobs: &mpsc::Receiver<Job>,
    out: &mpsc::Sender<Result<F, ReadFailure<E>>>,
) {
    use std::io::Write;
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
        let reply = read(&mut stdout, job.cap);
        let failed = reply.is_err();
        if out.send(reply).is_err() || failed {
            return;
        }
    }
}

/// What one spawn needs beyond the supervisor's fixed configuration.
pub(crate) struct SpawnRequest<'a> {
    /// Owner recorded in the process ledger.
    pub owner_session: &'a str,
    /// Arguments appended to the configured ones for this spawn only.
    pub extra_args: &'a [String],
    /// Dropped once the child is reaped (or at once when no child was started).
    pub keepalive: Option<Box<dyn Send>>,
    /// Largest first (HELLO) frame the reader may accept.
    pub hello_cap: u32,
}

/// Process supervisor. Cheap to clone: clones share one budget, ledger and counters.
#[derive(Clone)]
pub(crate) struct Supervisor {
    shared: Arc<Shared>,
}

impl Supervisor {
    pub(crate) fn new(config: SupervisorConfig) -> Self {
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

    pub(crate) fn stats(&self) -> SupervisorStats {
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
    pub(crate) fn ledger(&self) -> Vec<ProcessLedgerEntry> {
        lock(&self.shared.ledger).clone()
    }

    /// Reserves one worker and `bytes` or refuses at once; a partial acquisition is released.
    pub(crate) fn admit(&self, bytes: u64) -> Result<Permit, Refused> {
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

    /// Spawns a worker, registers it in the ledger and reads its first frame within the spawn timeout.
    /// From the moment the child exists, every error path drops the session, which kills and reaps it.
    /// `extra_args` follow the configured arguments for this spawn only; `keepalive` is dropped once
    /// the child is reaped (or at once when no child was started).
    pub(crate) fn spawn<F, E>(
        &self,
        permit: Permit,
        request: SpawnRequest<'_>,
        read: ReadFn<F, E>,
        cancel: &CancellationToken,
    ) -> Result<(Session<F, E>, F), SpawnError<E>>
    where
        F: Send + 'static,
        E: Send + 'static,
    {
        let SpawnRequest {
            owner_session,
            extra_args,
            keepalive,
            hello_cap,
        } = request;
        let shared = &self.shared;
        if cancel.check().is_err() {
            return Err(SpawnError::Canceled);
        }
        let mut command = Command::new(&shared.config.program);
        command
            .args(&shared.config.args)
            .args(extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_clear();
        quiet(&mut command);
        let mut child = command.spawn().map_err(|_| SpawnError::Unavailable)?;
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
        let (replies_tx, replies_rx) = mpsc::channel::<Result<F, ReadFailure<E>>>();
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
            _keepalive: keepalive,
            _permit: permit,
        };
        let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
            return Err(SpawnError::Unavailable);
        };
        thread::Builder::new()
            .name("score-plugin-worker-io".to_owned())
            .spawn(move || io_loop(stdin, stdout, read, hello_cap, &jobs_rx, &replies_tx))
            .map_err(|_| SpawnError::Unavailable)?;
        let wall = after(shared.config.spawn_timeout);
        match session.wait(cancel, wall) {
            Waited::Frame(first) => Ok((session, first)),
            Waited::Canceled => Err(session.handshake_failed(Failure::Canceled)),
            Waited::TimedOut => Err(session.handshake_failed(Failure::Deadline)),
            Waited::Failed(failure) => Err(session.handshake_failed(Failure::Read(failure))),
        }
    }
}

impl<F, E> Session<F, E> {
    fn handshake_failed(&mut self, failure: Failure<E>) -> SpawnError<E> {
        let (classified, done) = self.fail(failure);
        SpawnError::Handshake(classified, done)
    }

    fn wait(&self, cancel: &CancellationToken, wall: Instant) -> Waited<F, E> {
        loop {
            match self.replies.recv_timeout(WAIT_SLICE) {
                Ok(Ok(frame)) => return Waited::Frame(frame),
                Ok(Err(failure)) => return Waited::Failed(failure),
                Err(RecvTimeoutError::Disconnected) => return Waited::Failed(ReadFailure::Closed),
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

    /// Operating-system id of the worker process (also in the ledger).
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    /// False once the process was terminated or its stream is out of sync.
    pub(crate) fn is_usable(&self) -> bool {
        self.usable
    }

    /// Writes one request frame and waits for its reply frame (cancel and the deadline are polled).
    /// A failure does NOT reclaim the worker: pass it to [`Session::fail`].
    pub(crate) fn exchange(
        &mut self,
        frame: Vec<u8>,
        cap: u32,
        deadline: Duration,
        cancel: &CancellationToken,
    ) -> Result<F, Failure<E>> {
        if cancel.check().is_err() {
            return Err(Failure::Canceled);
        }
        if self.jobs.send(Job { frame, cap }).is_err() {
            return Err(Failure::Read(ReadFailure::Closed));
        }
        match self.wait(cancel, after(deadline)) {
            Waited::Frame(reply) => Ok(reply),
            Waited::Canceled => Err(Failure::Canceled),
            Waited::TimedOut => Err(Failure::Deadline),
            Waited::Failed(failure) => Err(Failure::Read(failure)),
        }
    }

    /// Reaps the child, killing it if it is still running after `patience`. Idempotent.
    pub(crate) fn terminate(&mut self, patience: Duration) -> Terminated {
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
    pub(crate) fn fail(&mut self, failure: Failure<E>) -> (Classified<E>, Terminated) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_arithmetic_never_overflows() {
        // Instant + Duration panics on overflow in std; `after` clamps instead (R6: spawn_timeout
        // and shutdown_grace were unbounded and reached `Instant::now() + limit`).
        let start = Instant::now();
        for wait in [
            Duration::MAX,
            Duration::from_secs(u64::MAX),
            Duration::from_secs(u64::MAX / 2),
            WAIT_CEILING,
        ] {
            let at = after(wait);
            assert!(at >= start, "{wait:?}");
            assert!(
                at <= Instant::now() + WAIT_CEILING + Duration::from_secs(1),
                "{wait:?} was not clamped"
            );
        }
        // A small wait is honoured exactly (within the clock), not clamped away.
        let soon = after(Duration::from_millis(250));
        assert!(soon >= start + Duration::from_millis(250));
        assert!(soon < start + Duration::from_secs(60));
    }

    #[test]
    fn worker_is_found_only_beside_the_running_executable() {
        // R6: the parent directory was also searched, so a binary planted one level up could become
        // the worker. Layout: <root>/parent/<name> (planted), <root>/parent/deps/test.exe.
        let root = std::env::temp_dir().join(format!(
            "hsk-score-plugin-discover-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let deps = root.join("parent").join("deps");
        std::fs::create_dir_all(&deps).unwrap();
        let name = format!("hsk-planted-worker{}", std::env::consts::EXE_SUFFIX);
        let exe = deps.join(format!("test{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&exe, b"x").unwrap();
        let planted_in_parent = root.join("parent").join(&name);
        std::fs::write(&planted_in_parent, b"planted").unwrap();

        let found = find_beside(&exe, "hsk-planted-worker");
        let beside = deps.join(&name);
        std::fs::write(&beside, b"own").unwrap();
        let found_beside = find_beside(&exe, "hsk-planted-worker");
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(
            found, None,
            "a parent-directory binary must not be accepted"
        );
        assert_eq!(found_beside, Some(beside));
    }
}
