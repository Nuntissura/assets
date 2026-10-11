//! Optional native plugin hosting provider; scoped process/lifetime/timeout and foreign resource
//! budgets separate from pure DSP.
//!
//! Model: the host crate NEVER loads, maps or probes foreign plugin code. Every plugin call runs in a
//! `score-plugin-worker` child process that [`IsolatedHost`] spawns, supervises (deadline, cancel,
//! crash, protocol faults), reclaims (kill + reap, permits released only after the reap) and records in
//! a host-owned process ledger. The caller supplies an [`ApprovedPlugin`] (path + SHA-256 + format +
//! architecture + ABI) and a [`PluginGrant`]; there is no discovery, no registry and no ambient scan.
//! The default build is pure Rust (`forbid(unsafe_code)`) and its reference worker backend is a
//! gain-and-delay processor: it proves isolation and lifecycle, not CLAP or VST3 (see [`DESCRIPTOR`]).
//!
//! Flow: `IsolatedHost::new` -> `probe` (optional) -> `activate` -> `process` / `set_parameter` /
//! `capture_state` -> `retire`. `PluginProcessor` lets a score `Engine` publish a hosted plugin's
//! block (the engine discards it on any failure).
// Default features are pure Rust and forbid `unsafe`. Feature `clap` adds the one module that loads a
// foreign library (`clap_backend`); `unsafe` is denied everywhere else and allowed only there.
#![cfg_attr(not(feature = "clap"), forbid(unsafe_code))]
#![cfg_attr(feature = "clap", deny(unsafe_code))]

mod backend;
#[cfg(feature = "clap")]
#[allow(unsafe_code)]
mod clap_backend;
mod error;
mod host;
mod processor;
mod report;
mod types;
mod wire;
mod worker;

pub use backend::REFERENCE_IDENTITY;
pub use error::{PluginError, ProtocolFault};
pub use host::{
    HostLimits, HostOptions, HostStats, IsolatedHost, IsolatedInstance, ProcessLedgerEntry,
    WorkerBackend,
};
pub use processor::PluginProcessor;
pub use report::{outcome_of, report};
pub use types::{
    ApprovedPlugin, Arch, Completeness, FaultReport, LifecycleState, MAX_DEADLINE, Parameter,
    PluginDescriptor, PluginFormat, PluginGrant, PluginInstance, PluginProvider, ProcessConfig,
    ProcessReceipt, RetireReport, SUPPORTED_CLAP_ABI, StateCapture,
};
pub use worker::{
    BACKEND_UNAVAILABLE_EXIT_CODE, FAULT_EXIT_CODE, PANIC_EXIT_CODE, WORKER_PROGRAM, worker_main,
};

/// Manual and inspection descriptor for a model or tool with no prior context.
pub const DESCRIPTOR: &str = r#"{"crate":"hsk-studio-score-plugin","schema":"hsk.studio.score_plugin.descriptor@1","purpose":"Optional native audio plugin hosting provider: scoped process, lifetime, timeout and foreign resource budgets, separate from pure DSP. Domain adapter only; the Handshake-wide plugin platform owns catalog, consent, scheduler and database.","proof_status":{"lifecycle_isolation":"PROVEN by the reference worker (gain and delay, pure Rust): spawn, deadline kill, crash, cancel, protocol faults, reap, ledger, permits","clap":"NOT_PROVEN until tests/clap_fixture.rs runs green: the native backend exists behind cargo feature clap (default OFF, clack-host 0.2.0 in the worker only) but no CLAP plugin ships on this machine; the fixture is a standalone clack-plugin gain 0.5 library under tests/fixtures/clap-gain built outside the workspace (path in HSK_SCORE_PLUGIN_CLAP_FIXTURE); even when green only a parameterless gain plugin ran: latency (extension not queried, reported 0), state (extension not wired, Unavailable), audio-port discovery and thread affinity are NOT_PROVEN","vst3":"NOT_PROVEN: declared in ApprovedPlugin, no backend (abi_mismatch)","os_resource_caps":"NOT_PROVEN: a Windows Job Object needs unsafe FFI; wall-clock kill and frame/block bounds are the enforced limits"},"isolation":{"model":"one worker child process per activated instance (probes use a short-lived worker); versioned length-prefixed protocol on the child's stdin/stdout; host holds the Child handles; no in-process fallback","spawn":"CREATE_NO_WINDOW on Windows, empty environment, null stderr, piped stdin/stdout","frame":{"header":"u32 magic 'HSPK', u16 version, u8 kind, u32 payload length (11 bytes)","bounds":"length checked against the cap before any payload allocation; unknown kind, short read, oversize, version skew and wrong request id are protocol faults","block":"planar f32 little-endian with explicit frame and channel counts"}},"authorization":{"approved_plugin":"path + SHA-256 + format + arch + abi_version, supplied by the caller","grant":"PluginGrant{actor, content_sha256} must name the same hash; none or a different hash is unauthorized before any process is spawned","content_check":"host reads the file (bounded) and compares the SHA-256 before spawning (hash_mismatch); the worker checks again before use","not_here":["discovery","directory scans","plugin catalog","consent store","editor windows"]},"flow":["IsolatedHost::new(HostOptions)","host.probe(&ApprovedPlugin, Some(&PluginGrant), &CancellationToken, deadline) -> PluginDescriptor","host.activate(&ProcessConfig, &cancel, deadline) -> Box<dyn PluginInstance>","instance.process(&InputBlock, &mut OutputBlock, &cancel, deadline) -> ProcessReceipt","instance.process_expecting(version, ..) refuses a block prepared under an older state_version (stale_state)","instance.set_parameter / capture_state(max_bytes) -> versioned Complete|Partial|Unavailable","instance.retire() -> RetireReport (killed, reaped, released_bytes)","PluginProcessor: score Processor over an instance; the Engine publishes or discards","report(result, correlation_id, revision, cancel, Observe, SinkPort)"],"lifecycle":{"states":["discovered","probed","active","processing","draining","retired"],"rule":"a call in the wrong state is illegal_transition; a crash, timeout, cancel or protocol fault leaves the instance draining (only retire is legal); a typed worker refusal (stale_state, budget, invalid config) keeps it active"},"limits":{"max_workers":"HostLimits.max_workers, refused at once with budget_exceeded (never waits)","max_in_flight_bytes":"2 x block bytes + max_state_bytes reserved per instance until the reap","max_frame_bytes":"bounds channels x max_block_frames x 4","deadline":"per call, 1 ms .. 60 s"},"backends":{"reference":"default: pure-Rust gain/delay, never loads the plugin file","clap":"HostOptions.backend = WorkerBackend::Clap; the worker binary must be built with cargo feature clap (otherwise it exits 3 and the host reports unavailable); src/clap_backend.rs is the only module with unsafe (PluginEntry::load); the first plugin of the entry is used"},"errors":["unauthorized","hash_mismatch","abi_mismatch","probe_timeout","crashed","timeout","stale_state","budget_exceeded","canceled","protocol","illegal_transition","unavailable","invalid_config","invalid_block","non_finite_output","plugin_fault"],"fault_output":"on any failure the output block holds silence, never partial plugin data; FaultReport{error, killed, reaped, exit_code, safe_output} is kept on the instance","recovery":{"crashed":"the worker was reaped; retire the instance and activate a new one","timeout":"the worker was killed and reaped; retire and activate again with a longer deadline if justified","stale_state":"capture_state to read the current version, rebuild the block, resubmit","budget_exceeded":"retire another instance or raise HostLimits; nothing waits","unavailable":"worker program missing or plugin file unreadable; fix the path"},"observe":"report() maps outcomes: Success, Canceled, Failure(Validation | Loss | Unavailable); no plugin path or sample data is attached","reference_worker":{"program":"score-plugin-worker","backend":"reference-gain-delay@1 (out[n] = in[n - latency] * gain; parameter 0 = gain in -16..16; state = gain)","test_args":"HostOptions.test_args (reference backend only): --latency N, --fault crash-on-block=N | hang-on-block=N | garbage-on-block=N | oversize-on-block=N | short-on-block=N | hang-on-probe | partial-state | no-state"},"not_in_scope":["in-process loading","realtime audio callback bridge (shared-memory handoff must follow STU-ARC-013)","VST3 or AU backends","editor or UI embedding","plugin install, update or uninstall","MIDI","parameter automation"]}"#;
