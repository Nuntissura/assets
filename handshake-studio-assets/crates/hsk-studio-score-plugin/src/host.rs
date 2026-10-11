//! Host side: `IsolatedHost`, a [`PluginProvider`] that runs every plugin call in a short-lived or
//! per-instance `score-plugin-worker` child process this crate spawns itself. The host process does not
//! load, map or probe foreign plugin code (test: `tests/clap_fixture.rs::clap_fixture_gain_runs_in_the_worker`), and there is no
//! in-process fallback.
//!
//! All process handling (spawn with an empty environment and no window, admission permits, ledger,
//! deadline/cancel/crash classification, kill and reap) lives in `supervisor.rs`; this file only gives
//! the plugin meaning to it: authorization, the plugin wire protocol, lifecycle, state versioning and
//! the typed errors.
//!
//! After a cancel, deadline, crash or protocol fault the output block holds silence instead of partial
//! plugin data (test: `tests/fault_injection.rs::process_faults_are_contained`) and the instance is `Draining`: only `retire` is legal.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{PluginError, ProtocolFault};
use crate::hashing;
use crate::stage_root::StageRoot;
use crate::supervisor::{
    Classified, Failure, Permit, ReadFailure, SpawnError, SpawnRequest, Supervisor,
    SupervisorConfig, Terminated, discover_program,
};
pub use crate::supervisor::{ProcessLedgerEntry, SupervisorStats as HostStats};
use crate::types::{
    ApprovedPlugin, Arch, Completeness, FaultReport, LifecycleState, MAX_DEADLINE, Parameter,
    PluginDescriptor, PluginFormat, PluginGrant, PluginInstance, PluginProvider, ProcessConfig,
    ProcessReceipt, RetireReport, SUPPORTED_CLAP_ABI, StateCapture,
};
use crate::wire::{self, ConfigureSpec, Frame, Hello, PluginSpec, Reply, WireError};
use crate::worker::{BACKEND_UNAVAILABLE_EXIT_CODE, WORKER_PROGRAM};
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::{ChannelLayout, InputBlock, OutputBlock};
use std::{
    path::PathBuf,
    process::ChildStdout,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

/// In-flight bytes admitted for a probe (no audio buffers).
const PROBE_BYTES: u64 = 64 << 10;

/// Host-wide bounds. [`IsolatedHost::new`] rejects zero values and any duration above
/// [`MAX_DEADLINE`] with `InvalidConfig` (test: `foundation_contract::host_limits_are_bounded_at_admission`).
#[derive(Clone, Debug)]
pub struct HostLimits {
    /// Most worker processes alive at once (probes included); a call beyond that is refused at once.
    pub max_workers: usize,
    /// Most bytes reserved across all live workers: block and state buffers plus a fixed 64 KiB hashing
    /// buffer per instance (the plugin file is streamed, its size is not charged).
    pub max_in_flight_bytes: u64,
    /// Largest frame either side accepts; bounds `max_block_frames * channels * 4`.
    pub max_frame_bytes: u32,
    /// Largest approved plugin file the host will hash (streamed over a fixed buffer).
    pub max_plugin_file_bytes: u64,
    /// Time a new worker gets to complete its HELLO handshake (1 ms ..= [`MAX_DEADLINE`]).
    pub spawn_timeout: Duration,
    /// Time retire waits for the worker to exit by itself before killing it (1 ms ..= [`MAX_DEADLINE`]).
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
    /// Pure-Rust gain-and-delay reference (reads and hashes the plugin file, does not load it). Exercises isolation only.
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
    /// Worker executable. `None`: `score-plugin-worker` in the running executable's own directory
    /// (no other directory is searched; test and example binaries pass the program explicitly).
    pub program: Option<PathBuf>,
    pub limits: HostLimits,
    pub backend: WorkerBackend,
    /// Directory under which the host makes its private staging directory (only used with
    /// `WorkerBackend::Clap`, whose worker loads a verified copy of the plugin instead of the approved
    /// path). `None`: the host process's temp directory.
    pub staging_root: Option<PathBuf>,
    /// Delay, in samples, the reference backend applies (0 = none). Ignored by the CLAP backend.
    pub reference_latency: u32,
    /// Extra worker command-line arguments for fault injection (`--fault SPEC`). Exists only in
    /// builds with the non-default cargo feature `test-faults`; no fault code ships otherwise.
    #[cfg(feature = "test-faults")]
    pub test_args: Vec<String>,
}

impl HostOptions {
    pub fn with_worker_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = Some(program.into());
        self
    }
}

struct Shared {
    supervisor: Supervisor,
    limits: HostLimits,
    next_instance: AtomicU64,
    /// Private staging directory for the CLAP backend (`None` for the reference backend, which never
    /// loads the file).
    stage: Option<Arc<StageRoot>>,
}

type WorkerSession = crate::supervisor::Session<Frame, ProtocolFault>;
type WorkerFailure = Failure<ProtocolFault>;

fn mismatch() -> WorkerFailure {
    Failure::Read(ReadFailure::Failed(ProtocolFault::UnexpectedKind))
}

fn closed() -> WorkerFailure {
    Failure::Read(ReadFailure::Closed)
}

fn framing(error: WireError) -> WorkerFailure {
    Failure::Read(ReadFailure::Failed(
        error.fault().unwrap_or(ProtocolFault::Inconsistent),
    ))
}

/// The supervisor's frame reader for this protocol.
fn read_reply(stdout: &mut ChildStdout, cap: u32) -> Result<Frame, ReadFailure<ProtocolFault>> {
    wire::read_frame(stdout, cap).map_err(|error| match error.fault() {
        None => ReadFailure::Closed,
        Some(fault) => ReadFailure::Failed(fault),
    })
}

fn error_of(classified: Classified<ProtocolFault>, probe: bool) -> PluginError {
    match classified {
        Classified::Canceled => PluginError::Canceled,
        Classified::Deadline if probe => PluginError::ProbeTimeout,
        Classified::Deadline => PluginError::Timeout,
        Classified::Crashed { exit_code } => PluginError::Crashed { exit_code },
        Classified::Protocol(fault) => PluginError::Protocol(fault),
    }
}

/// Reclaims the worker for `failure` and returns the typed error plus what happened to the process.
fn fail_session(
    session: &mut WorkerSession,
    failure: WorkerFailure,
    probe: bool,
) -> (PluginError, Terminated) {
    let (classified, done) = session.fail(failure);
    (error_of(classified, probe), done)
}

/// One request/reply with the protocol's request-id check. A failure does not reclaim the worker.
fn call(
    session: &mut WorkerSession,
    id: u64,
    frame: Vec<u8>,
    cap: u32,
    deadline: Duration,
    cancel: &CancellationToken,
) -> Result<Reply, WorkerFailure> {
    let received = session.exchange(frame, cap, deadline, cancel)?;
    let reply = wire::decode_reply(&received).map_err(framing)?;
    if reply.id() == id {
        Ok(reply)
    } else {
        Err(mismatch())
    }
}

/// A started worker and what it announced.
struct Started {
    session: WorkerSession,
    hello: Hello,
}

/// Spawns a worker and completes the HELLO handshake. Every error path leaves the child reaped.
fn start_worker(
    shared: &Shared,
    permit: Permit,
    owner_session: &str,
    expected_clap_abi: u32,
    cancel: &CancellationToken,
) -> Result<Started, PluginError> {
    // The CLAP worker loads a verified private copy, never the approved path: it gets a directory of its
    // own, removed after the reap (the supervisor drops the guard with the session).
    let (extra_args, keepalive): (Vec<String>, Option<Box<dyn Send>>) = match &shared.stage {
        None => (Vec::new(), None),
        Some(root) => {
            let stage = root.worker_dir().map_err(|_| PluginError::Unavailable)?;
            let dir = stage
                .path()
                .to_str()
                .ok_or(PluginError::InvalidConfig)?
                .to_owned();
            (
                vec!["--stage-dir".to_owned(), dir],
                Some(Box::new(stage) as Box<dyn Send>),
            )
        }
    };
    let (mut session, first) = match shared.supervisor.spawn(
        permit,
        SpawnRequest {
            owner_session,
            extra_args: &extra_args,
            keepalive,
            hello_cap: wire::HELLO_CAP,
        },
        read_reply,
        cancel,
    ) {
        Ok(started) => started,
        Err(SpawnError::Canceled) => return Err(PluginError::Canceled),
        Err(SpawnError::Unavailable) => return Err(PluginError::Unavailable),
        Err(SpawnError::Handshake(classified, done)) => {
            // A worker that exits at once with the "backend not built" code never said HELLO.
            let unbuilt = matches!(classified, Classified::Crashed { .. })
                && done.exit_code == Some(BACKEND_UNAVAILABLE_EXIT_CODE);
            return Err(if unbuilt {
                PluginError::Unavailable
            } else {
                error_of(classified, true)
            });
        }
    };
    let hello = match wire::decode_hello(&first) {
        Ok(hello) => hello,
        Err(error) => return Err(fail_session(&mut session, framing(error), true).0),
    };
    if hello.clap_abi != expected_clap_abi {
        session.terminate(Duration::ZERO);
        return Err(PluginError::AbiMismatch);
    }
    Ok(Started { session, hello })
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

/// Checks the approved file against the per-file bound (a regular file no larger than
/// `max_bytes`) and returns the bound to read it under.
fn plugin_file_bound(plugin: &ApprovedPlugin, max_bytes: u64) -> Result<u64, PluginError> {
    let meta = std::fs::metadata(&plugin.path).map_err(|_| PluginError::Unavailable)?;
    if !meta.is_file() {
        return Err(PluginError::Unavailable);
    }
    if meta.len() > max_bytes {
        return Err(PluginError::BudgetExceeded);
    }
    Ok(max_bytes)
}

/// Streams the approved file through SHA-256 over one fixed buffer (`hashing::BUFFER_BYTES`, never the
/// whole file) and compares the digest with the approval. This is a read, not a load: the host does not
/// map or execute the file. It is an early refusal only; the worker hashes the bytes it will actually
/// load (see `staging.rs`). A file that grows past `max_bytes` while it is read is refused.
fn verify_content(plugin: &ApprovedPlugin, max_bytes: u64) -> Result<(), PluginError> {
    let mut file = std::fs::File::open(&plugin.path).map_err(|_| PluginError::Unavailable)?;
    let (digest, _) =
        hashing::stream_sha256(&mut file, None, max_bytes).map_err(|error| match error {
            hashing::HashError::TooLarge => PluginError::BudgetExceeded,
            hashing::HashError::Io => PluginError::Unavailable,
        })?;
    if digest == plugin.content_sha256 {
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
            || limits.spawn_timeout > MAX_DEADLINE
            || limits.shutdown_grace.is_zero()
            || limits.shutdown_grace > MAX_DEADLINE
        {
            return Err(PluginError::InvalidConfig);
        }
        let program = options
            .program
            .clone()
            .or_else(|| discover_program(WORKER_PROGRAM))
            .filter(|path| path.is_file())
            .ok_or(PluginError::Unavailable)?;
        let mut args = Vec::new();
        if options.backend == WorkerBackend::Clap {
            args.extend(["--backend".to_owned(), "clap".to_owned()]);
        }
        if options.reference_latency != 0 {
            args.extend([
                "--latency".to_owned(),
                options.reference_latency.to_string(),
            ]);
        }
        #[cfg(feature = "test-faults")]
        args.extend(options.test_args.iter().cloned());
        let stage = if options.backend == WorkerBackend::Clap {
            let base = options
                .staging_root
                .clone()
                .unwrap_or_else(std::env::temp_dir);
            Some(Arc::new(
                StageRoot::create(&base).map_err(|_| PluginError::Unavailable)?,
            ))
        } else {
            None
        };
        let supervisor = Supervisor::new(SupervisorConfig {
            program,
            args,
            max_workers: options.limits.max_workers,
            max_in_flight_bytes: options.limits.max_in_flight_bytes,
            spawn_timeout: options.limits.spawn_timeout,
        });
        Ok(Self {
            shared: Arc::new(Shared {
                supervisor,
                limits: options.limits,
                next_instance: AtomicU64::new(1),
                stage,
            }),
        })
    }

    pub fn stats(&self) -> HostStats {
        self.shared.supervisor.stats()
    }

    /// Snapshot of every process this host ever spawned.
    pub fn ledger(&self) -> Vec<ProcessLedgerEntry> {
        self.shared.supervisor.ledger()
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
        let block_bytes = channels
            .checked_mul(config.max_block_frames)
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| u64::try_from(n).ok())
            .ok_or(PluginError::BudgetExceeded)?;
        let reply_cap = block_bytes
            .checked_add(u64::from(wire::REPLY_SLACK))
            .and_then(|n| u32::try_from(n).ok())
            .ok_or(PluginError::BudgetExceeded)?;
        let limits = &self.shared.limits;
        if reply_cap > limits.max_frame_bytes
            || config.max_state_bytes.saturating_add(wire::REPLY_SLACK) > limits.max_frame_bytes
        {
            return Err(PluginError::BudgetExceeded);
        }
        // Request and reply buffers, the largest state blob and the fixed plugin-file hashing buffer
        // stay reserved until the reap. The plugin file itself is streamed: its size is not charged.
        let file_bound = plugin_file_bound(&config.plugin, limits.max_plugin_file_bytes)?;
        let reserved = block_bytes
            .checked_mul(2)
            .and_then(|n| n.checked_add(u64::from(config.max_state_bytes)))
            .and_then(|n| n.checked_add(hashing::BUFFER_BYTES as u64))
            .ok_or(PluginError::BudgetExceeded)?;
        let permit = self
            .shared
            .supervisor
            .admit(reserved)
            .map_err(|_| PluginError::BudgetExceeded)?;
        verify_content(&config.plugin, file_bound)?;
        let spec = spec_of(&config.plugin)?;
        let Started { mut session, .. } = start_worker(
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
        let reply = call(
            &mut session,
            request_id,
            frame,
            wire::REPLY_SLACK,
            deadline,
            cancel,
        );
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
            Ok(_) => return Err(fail_session(&mut session, mismatch(), true).0),
            Err(failure) => return Err(fail_session(&mut session, failure, true).0),
        };
        let state = LifecycleState::Discovered
            .transition(LifecycleState::Probed)
            .and_then(|s| s.transition(LifecycleState::Active))?;
        let pid = Some(session.pid());
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
            shutdown_grace: limits.shutdown_grace,
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
        let file_bound = plugin_file_bound(plugin, limits.max_plugin_file_bytes)?;
        let permit = self
            .shared
            .supervisor
            .admit(PROBE_BYTES)
            .map_err(|_| PluginError::BudgetExceeded)?;
        verify_content(plugin, file_bound)?;
        let spec = spec_of(plugin)?;
        let Started { mut session, hello } =
            start_worker(&self.shared, permit, &owner, plugin.abi_version, cancel)?;
        let frame = wire::encode_probe(1, &spec);
        let result = match call(&mut session, 1, frame, wire::REPLY_SLACK, deadline, cancel) {
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
                    provider_identity: hello.identity.clone(),
                })
                .ok_or(PluginError::Protocol(ProtocolFault::Inconsistent)),
            Ok(Reply::Error { code, .. }) => Err(wire::error_of(code)),
            Ok(_) => Err(fail_session(&mut session, mismatch(), true).0),
            Err(failure) => Err(fail_session(&mut session, failure, true).0),
        };
        // A probe worker is short-lived: ask it to leave, then reap it either way.
        if session.is_usable() {
            let _ = call(
                &mut session,
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
    session: Option<WorkerSession>,
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
    shutdown_grace: Duration,
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
        self.next_request = self.next_request.checked_add(1).unwrap_or(1);
        id
    }

    /// Records a killed or crashed worker: the instance can only be retired from here.
    fn fail(&mut self, failure: WorkerFailure, silenced: bool) -> PluginError {
        let (error, done) = match self.session.as_mut() {
            Some(session) => fail_session(session, failure, false),
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
            let grace = self.shutdown_grace;
            if session.is_usable() {
                let id = self.next_request;
                let _ = call(
                    &mut session,
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
        let mut samples = Vec::with_capacity(channels.saturating_mul(frames));
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
        let cap = channels
            .checked_mul(frames)
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| u32::try_from(n).ok())
            .and_then(|b| b.checked_add(wire::REPLY_SLACK))
            .ok_or(PluginError::BudgetExceeded)?;
        self.state = LifecycleState::Processing;
        let Some(session) = self.session.as_mut() else {
            silence(out, frames);
            return Err(self.fail(closed(), true));
        };
        let reply = call(session, id, frame, cap, deadline, cancel);
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
                    && processed.len() == channels.saturating_mul(frames)
                    && completeness.is_some()
                    && latency == self.latency_samples;
                let Some(completeness) = completeness.filter(|_| consistent) else {
                    silence(out, frames);
                    return Err(self.fail(mismatch(), true));
                };
                if processed.iter().any(|s| !s.is_finite()) {
                    silence(out, frames);
                    return Err(self.refuse(PluginError::NonFiniteOutput, true));
                }
                // `frames` is non-zero (the input block was validated) and `processed` holds
                // exactly `channels * frames` samples (checked above).
                for (src, dst) in processed.chunks_exact(frames).zip(out.channels_mut()) {
                    dst[..frames].copy_from_slice(src);
                }
                self.state = LifecycleState::Active;
                self.blocks = self.blocks.saturating_add(1);
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
            Ok(Reply::Error { code, .. }) => {
                silence(out, frames);
                if code == wire::E_STALE {
                    // The host sent its own current version (checked above), so a worker that calls
                    // it stale disagrees with the host's counter. The host owns that counter and
                    // never adopts a number from the worker: the worker is inconsistent, reclaim it
                    // (test: `fault_injection::worker_cannot_rewrite_the_state_version`).
                    return Err(self.fail(mismatch(), true));
                }
                Err(self.refuse(wire::error_of(code), true))
            }
            Ok(_) => {
                silence(out, frames);
                Err(self.fail(mismatch(), true))
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
            return Err(self.fail(closed(), false));
        };
        match call(session, id, frame, wire::REPLY_SLACK, deadline, cancel) {
            Ok(Reply::ParamSet { version, .. }) => {
                // One parameter change is exactly one version step; any other number is the worker
                // rewriting the host's counter.
                if self.state_version.checked_add(1) == Some(version) {
                    self.state_version = version;
                    Ok(version)
                } else {
                    Err(self.fail(mismatch(), false))
                }
            }
            Ok(Reply::Error { code, .. }) => Err(self.refuse(wire::error_of(code), false)),
            Ok(_) => Err(self.fail(mismatch(), false)),
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
            return Err(self.fail(closed(), false));
        };
        let cap = wire::REPLY_SLACK.saturating_add(max_bytes);
        match call(session, id, frame, cap, deadline, cancel) {
            Ok(Reply::State {
                version,
                completeness,
                bytes,
                ..
            }) => {
                let known = Completeness::from_wire(completeness);
                let fits = bytes.len() <= usize::try_from(max_bytes).unwrap_or(0);
                match known {
                    // A capture reports the current version; it does not change it.
                    Some(completeness) if fits && version == self.state_version => {
                        Ok(StateCapture {
                            state_version: version,
                            completeness,
                            bytes,
                        })
                    }
                    Some(_) if fits => Err(self.fail(mismatch(), false)),
                    _ => Err(self.fail(
                        Failure::Read(ReadFailure::Failed(ProtocolFault::Oversize)),
                        false,
                    )),
                }
            }
            Ok(Reply::Error { code, .. }) => Err(self.refuse(wire::error_of(code), false)),
            Ok(_) => Err(self.fail(mismatch(), false)),
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
