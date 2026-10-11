//! Optional native plugin hosting provider; scoped process/lifetime/timeout and foreign resource
//! budgets separate from pure DSP.
//!
//! Model: the host crate does not load, map or probe foreign plugin code (the loader dependency exists
//! only behind feature `clap` in the worker; test: `tests/clap_fixture.rs::clap_fixture_gain_runs_in_the_worker` reads the host process's
//! module list from the operating system). Every plugin call runs in a
//! `score-plugin-worker` child process that [`IsolatedHost`] spawns, supervises (deadline, cancel,
//! crash, protocol faults), reclaims (kill + reap, permits released only after the reap) and records in
//! a host-owned process ledger. The caller supplies an [`ApprovedPlugin`] (path + SHA-256 + format +
//! architecture + ABI) and a [`PluginGrant`]; there is no discovery, no registry and no ambient scan.
//! The default build is pure Rust (`forbid(unsafe_code)`) and its reference worker backend is a
//! gain-and-delay processor: it exercises isolation and lifecycle (`tests/foundation_contract.rs`,
//! `tests/fault_injection.rs`), not CLAP or VST3 (see [`DESCRIPTOR`]).
//!
//! Flow: `IsolatedHost::new` -> `probe` (optional) -> `activate` -> `process` / `set_parameter` /
//! `capture_state` -> `retire`. `PluginProcessor` lets a score `Engine` publish a hosted plugin's
//! block (the engine discards it on any failure).
// Default features are pure Rust and forbid `unsafe`. Feature `clap` adds the one module that loads a
// foreign library (`clap_backend`); `unsafe` is denied everywhere else and allowed only there.
// Test builds of the library deny instead of forbid so the allocation meter in `hashing.rs` (a test-only
// global allocator) can exist; no non-test build of the default feature set contains `unsafe`.
#![cfg_attr(not(any(feature = "clap", test)), forbid(unsafe_code))]
#![cfg_attr(any(feature = "clap", test), deny(unsafe_code))]

mod backend;
#[cfg(feature = "clap")]
#[allow(unsafe_code)]
mod clap_backend;
mod error;
#[cfg(feature = "test-faults")]
mod faults;
mod hashing;
mod host;
mod processor;
mod report;
mod stage_root;
#[cfg(any(feature = "clap", test))]
mod staging;
mod supervisor;
mod types;
mod wire;
mod worker;

pub use backend::REFERENCE_IDENTITY;
pub use error::{PluginError, ProtocolFault};
#[cfg(feature = "test-faults")]
pub use faults::FAULT_EXIT_CODE;
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
pub use worker::{BACKEND_UNAVAILABLE_EXIT_CODE, PANIC_EXIT_CODE, WORKER_PROGRAM, worker_main};

/// Manual and inspection descriptor for a model or tool with no prior context.
pub const DESCRIPTOR: &str = r#"{"crate":"hsk-studio-score-plugin","schema":"hsk.studio.score_plugin.descriptor@1","purpose":"Optional native audio plugin hosting provider: scoped process, lifetime, timeout and foreign resource budgets, separate from pure DSP. Domain adapter only; the Handshake-wide plugin platform owns catalog, consent, scheduler and database.","proof_status":{"lifecycle_isolation":"exercised with the reference worker (gain and delay, pure Rust) by tests/foundation_contract.rs (spawn, reap, ledger, permits, authorization, state versions) and tests/fault_injection.rs (feature test-faults: deadline kill, crash, cancel, protocol faults)","clap":"NOT_PROVEN until tests/clap_fixture.rs runs green: the native backend exists behind cargo feature clap (default OFF, clack-host 0.2.0 in the worker only) but no CLAP plugin ships on this machine; the fixture is a standalone clack-plugin gain 0.5 library under tests/fixtures/clap-gain built outside the workspace by tools/run-round.sh into the oracle corpus (score-plugin/clap-gain/MANIFEST.json via .config/oracle-corpus.toml; optional local override HSK_SCORE_PLUGIN_CLAP_FIXTURE, never required; absent or different hash prints NOT_PROVEN:); even when green only a parameterless gain plugin ran: latency (extension not queried, reported 0), state (extension not wired, Unavailable), audio-port discovery and thread affinity are NOT_PROVEN","vst3":"NOT_PROVEN: declared in ApprovedPlugin, no backend (abi_mismatch)","os_resource_caps":"NOT_PROVEN: a Windows Job Object needs unsafe FFI; wall-clock kill and frame/block bounds are the enforced limits"},"isolation":{"model":"one worker child process per activated instance (probes use a short-lived worker); versioned length-prefixed protocol on the child's stdin/stdout; host holds the Child handles; no in-process fallback","spawn":"CREATE_NO_WINDOW on Windows, empty environment, null stderr, piped stdin/stdout","frame":{"header":"u32 magic 'HSPK', u16 version, u8 kind, u32 payload length (11 bytes)","bounds":"length checked against the cap before any payload allocation; unknown kind, short read, oversize, version skew and wrong request id are protocol faults","block":"planar f32 little-endian with explicit frame and channel counts"}},"authorization":{"approved_plugin":"path + SHA-256 + format + arch + abi_version, supplied by the caller","grant":"PluginGrant{actor, content_sha256} must name the same hash; none or a different hash is unauthorized before any process is spawned","content_check":"host streams the file through SHA-256 over one fixed 64 KiB buffer (bounded by HostLimits.max_plugin_file_bytes) and compares the digest before spawning (hash_mismatch): an early refusal only. The authoritative check is the worker's: the CLAP worker hashes the bytes it read through one handle, writes exactly those bytes to a private staging file it creates exclusively inside a directory the host made for it, hashes the COPY again through a handle it keeps open, and gives the dynamic loader the copy's path, never the approved path (tests: staging::tests::tampering_after_staging_cannot_reach_the_copy, staging::tests::copy_cannot_be_modified_while_held on Windows, clap_fixture::clap_loads_a_private_verified_copy_not_the_approved_path)",
"hash_then_load":"closed for the CLAP backend by the staging copy. NOT_PROVEN: on Unix a process of the same user that can write the private 0700 directory between the copy's verification and dlopen (it already holds the plugin's privileges; Windows denies write and delete through the kept share-mode handle); a plugin that needs sibling files or helper libraries next to it does not work from a lone staged file (siblings carry no approved hash); the reference backend hashes the path and never loads it",
"hash_buffer":"the plugin file is never read whole: host (verify_content), reference worker (verify_file) and the CLAP staging copy (stage_verified) stream it through sha2 0.10.9 (force-soft) over one 64 KiB buffer, so peak memory does not depend on the file size; the digest equals the one-shot sha256 of hsk-studio-score (tests: hashing::tests::{equals_the_one_shot_digest, large_stream_hashes_in_bounded_memory}, tests/hash_memory.rs::host_hashes_a_large_plugin_file_in_bounded_memory, staging::tests::large_file_is_staged_in_bounded_memory, backend::tests::reference_verify_streams_a_large_file_in_bounded_memory; 48-96 MiB files, per-thread allocation meter, peak under 4 MiB). Not covered: operating-system memory the loader maps for the library itself, and any allocation inside the plugin",
"not_here":["discovery","directory scans","plugin catalog","consent store","editor windows"]},"flow":["IsolatedHost::new(HostOptions)","host.probe(&ApprovedPlugin, Some(&PluginGrant), &CancellationToken, deadline) -> PluginDescriptor","host.activate(&ProcessConfig, &cancel, deadline) -> Box<dyn PluginInstance>","instance.process(&InputBlock, &mut OutputBlock, &cancel, deadline) -> ProcessReceipt","instance.process_expecting(version, ..) refuses a block prepared under an older state_version (stale_state)","instance.set_parameter / capture_state(max_bytes) -> versioned Complete|Partial|Unavailable","instance.retire() -> RetireReport (killed, reaped, released_bytes)","PluginProcessor: score Processor over an instance; the Engine publishes or discards","report(result, correlation_id, revision, cancel, Observe, SinkPort)"],"lifecycle":{"states":["discovered","probed","active","processing","draining","retired"],"rule":"a call in the wrong state is illegal_transition; a crash, timeout, cancel or protocol fault leaves the instance draining (only retire is legal); a typed worker refusal (stale_state, budget, invalid config) keeps it active"},"limits":{"max_workers":"HostLimits.max_workers, refused at once with budget_exceeded, no wait (test: foundation_contract::admission_and_cancel)","max_in_flight_bytes":"2 x block bytes + max_state_bytes + the fixed 64 KiB hashing buffer reserved per instance (probe: 64 KiB) until the reap; the plugin file size is not charged because the file is streamed (test: foundation_contract::plugin_file_bound_applies_and_its_size_is_not_charged)",
"durations":"HostLimits.spawn_timeout and shutdown_grace must be 1 ms ..= MAX_DEADLINE (60 s) or IsolatedHost::new returns invalid_config; per-call deadlines have the same range; internal waits are clamped so Instant arithmetic cannot overflow (tests: foundation_contract::host_limits_are_bounded_at_admission, supervisor::tests::deadline_arithmetic_never_overflows)","max_frame_bytes":"bounds channels x max_block_frames x 4","deadline":"per call, 1 ms .. 60 s"},"backends":{"reference":"default: pure-Rust gain/delay; reads and hashes the plugin file, does not load it","clap":"HostOptions.backend = WorkerBackend::Clap; the worker binary must be built with cargo feature clap (otherwise it exits 3 and the host reports unavailable); src/clap_backend.rs is the only module with unsafe (PluginEntry::load, called on the verified staging copy); the first plugin of the entry is used; HostOptions.staging_root picks where the host makes its private staging directory (default: the host process temp directory; removed with the host, a worker's subdirectory is removed after its reap: test foundation_contract::staging_directories_follow_the_worker)"},"errors":["unauthorized","hash_mismatch","abi_mismatch","probe_timeout","crashed","timeout","stale_state","budget_exceeded","canceled","protocol","illegal_transition","unavailable","invalid_config","invalid_block","non_finite_output","plugin_fault"],"fault_output":"on any failure the output block holds silence instead of partial plugin data (tests: fault_injection::process_faults_are_contained, state_completeness_and_cancel_mid_process); FaultReport{error, killed, reaped, exit_code, safe_output} is kept on the instance","recovery":{"crashed":"the worker was reaped; retire the instance and activate a new one","timeout":"the worker was killed and reaped; retire and activate again with a longer deadline if justified","stale_state":"capture_state to read the current version, rebuild the block, resubmit","budget_exceeded":"retire another instance or raise HostLimits; nothing waits","unavailable":"worker program missing or plugin file unreadable; fix the path"},"observe":"report() maps outcomes: Success, Canceled, Failure(Validation | Loss | Unavailable); no plugin path or sample data is attached","reference_worker":{"program":"score-plugin-worker","backend":"reference-gain-delay@1 (out[n] = in[n - latency] * gain; parameter 0 = gain in -16..16; state = gain)","test_args":"HostOptions.reference_latency sets --latency N (reference backend); fault injection exists only with the non-default cargo feature test-faults: HostOptions.test_args = --fault crash-on-block=N | hang-on-block=N | garbage-on-block=N | oversize-on-block=N | short-on-block=N | hang-on-probe | partial-state | no-state | lie-param-version | lie-capture-version | stale-on-block=N"},"trust_boundary":{"worker_program":"HostOptions.program, else score-plugin-worker in the running executable's own directory only; no parent directory is searched (test: supervisor::tests::worker_is_found_only_beside_the_running_executable)","state_version":"the host owns the counter: set_parameter must be answered with exactly host version + 1, a state capture with the host version, and a worker that calls the host's own version stale is inconsistent; each is a protocol fault that reclaims the worker and leaves the host counter untouched (test: fault_injection::worker_cannot_rewrite_the_state_version)"},"not_in_scope":["in-process loading","realtime audio callback bridge (shared-memory handoff must follow STU-ARC-013)","VST3 or AU backends","editor or UI embedding","plugin install, update or uninstall","MIDI","parameter automation"]}"#;
