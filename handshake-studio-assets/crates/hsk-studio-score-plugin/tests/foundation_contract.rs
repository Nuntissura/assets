//! Process-isolation contract of the Score Plugin provider, proven with the REFERENCE worker (a pure
//! Rust gain-and-delay processor behind the real protocol). This proves spawn, deadline, crash,
//! cancel, protocol-fault, reap and budget behaviour; it does NOT prove CLAP or VST3 (MT-35560: a mock
//! host does not prove a plugin format; no plugin fixture exists, so CLAP stays NOT_PROVEN).
//!
//! Oracles the worker cannot edit: the gain/delay arithmetic is recomputed in the test from the input
//! signal (exact binary fractions, so equality is bit-exact); the approved-file hash is the NIST
//! SHA-256 vector of "abc"; worker liveness is read from the operating system (`tasklist`), not from the
//! host's own bookkeeping. Every child is one this test's host spawned; nothing opens a window.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, Error as ObserveError, FailureCode, MAX_FRAME_BYTES,
    Observe, Outcome, SinkPort,
};
use hsk_studio_score::{
    BlockSpec, ChannelLayout, Engine, InputBlock, OutputBlock, RequestContext, ScoreError,
};
use hsk_studio_score_plugin::*;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

const WORKER: &str = env!("CARGO_BIN_EXE_score-plugin-worker");
const RESOURCE: &str = "STRK-019abcde-0000-7000-8000-000000000001";
/// NIST FIPS 180-4 SHA-256("abc").
const ABC_SHA256: [u8; 32] = [
    0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23,
    0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
];
const DEADLINE: Duration = Duration::from_secs(10);

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
    };
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 120s deadline: {e}"),
    }
}

/// The approved "plugin" file ("abc"), removed when dropped (the test creates it, the test removes it).
struct PluginFile(PathBuf);

impl PluginFile {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "hsk-score-plugin-{}-{}.bin",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::AcqRel)
        ));
        std::fs::write(&path, b"abc").unwrap();
        Self(path)
    }
    fn approved(&self) -> ApprovedPlugin {
        ApprovedPlugin {
            path: self.0.clone(),
            content_sha256: ABC_SHA256,
            format: PluginFormat::Clap,
            arch: Arch::host().expect("known arch"),
            abi_version: SUPPORTED_CLAP_ABI,
        }
    }
}

impl Drop for PluginFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn actor() -> ActorContext {
    ActorContext::new(
        "account",
        "principal",
        "owner",
        "owner-principal",
        "space",
        "session-7",
    )
    .unwrap()
}

fn grant() -> PluginGrant {
    PluginGrant {
        actor: actor(),
        content_sha256: ABC_SHA256,
    }
}

fn host_with(args: &[&str], limits: HostLimits) -> IsolatedHost {
    let mut options = HostOptions::default().with_worker_program(WORKER);
    options.limits = limits;
    options.test_args = args.iter().map(|a| (*a).to_owned()).collect();
    IsolatedHost::new(options).unwrap()
}

fn host(args: &[&str]) -> IsolatedHost {
    host_with(args, HostLimits::default())
}

fn config(file: &PluginFile, gain: f64) -> ProcessConfig {
    ProcessConfig {
        plugin: file.approved(),
        grant: Some(grant()),
        sample_rate_hz: 48_000,
        layout: ChannelLayout::stereo(),
        max_block_frames: 256,
        parameters: vec![Parameter { id: 0, value: gain }],
        max_state_bytes: 64,
    }
}

/// L[n] = ((n mod 251) / 256) - 0.5, R = -L: exact binary fractions, so gain 0.5 / 0.25 and the delay
/// are exact in f32.
fn signal(frames: usize) -> (Vec<f32>, Vec<f32>) {
    let left: Vec<f32> = (0..frames)
        .map(|n| (n % 251) as f32 / 256.0 - 0.5)
        .collect();
    let right = left.iter().map(|s| -s).collect();
    (left, right)
}

fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|s| s.to_bits()).collect()
}

fn spec(frames: usize, start_sample: u64) -> BlockSpec {
    BlockSpec {
        sample_rate_hz: 48_000,
        layout: ChannelLayout::stereo(),
        frames,
        start_sample,
    }
}

/// Operating-system view of a worker's liveness (not the host's bookkeeping).
fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .creation_flags(0x0800_0000)
            .output()
            .expect("tasklist runs");
        String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
    }
    #[cfg(not(windows))]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
}

fn silent(out: &OutputBlock, frames: usize) -> bool {
    (0..out.layout().channels()).all(|ch| {
        out.backing().chunks(out.capacity()).nth(ch).unwrap()[..frames]
            .iter()
            .all(|s| *s == 0.0)
    })
}

fn prefilled(frames: usize) -> OutputBlock {
    let mut out = OutputBlock::with_capacity(ChannelLayout::stereo(), frames).unwrap();
    for channel in out.channels_mut() {
        channel.fill(7.0);
    }
    out
}

#[test]
fn worker_gain_block_exact() {
    bounded_case("worker_gain_block_exact", || {
        let file = PluginFile::new();
        let host = host(&["--latency", "3"]);
        let cancel = CancellationToken::default();
        let descriptor = host
            .probe(&file.approved(), Some(&grant()), &cancel, DEADLINE)
            .unwrap();
        assert_eq!(
            (descriptor.format, descriptor.abi_version, descriptor.inputs),
            (PluginFormat::Clap, 1, 2)
        );
        let mut instance = host
            .activate(&config(&file, 0.5), &cancel, DEADLINE)
            .unwrap();
        assert_eq!(
            (
                instance.lifecycle(),
                instance.state_version(),
                instance.latency_samples()
            ),
            (LifecycleState::Active, 1, 3)
        );

        // Two consecutive 256-frame blocks through a score Engine; expected output is the whole 512-sample
        // signal delayed by 3 and halved, recomputed here.
        let (left, right) = signal(512);
        let delayed = |src: &[f32]| -> Vec<f32> {
            (0..512)
                .map(|n| if n < 3 { 0.0 } else { src[n - 3] * 0.5 })
                .collect()
        };
        let (want_l, want_r) = (delayed(&left), delayed(&right));
        let resource = DomainId::parse(RESOURCE).unwrap();
        let engine = Engine::new(resource.clone(), 7);
        let ctx = RequestContext {
            resource,
            actor: Some(actor()),
            expected_revision: 7,
        };
        let mut out = OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap();
        for block in 0..2_usize {
            let range = block * 256..(block + 1) * 256;
            let sp = spec(256, (block * 256) as u64);
            let channels: [&[f32]; 2] = [&left[range.clone()], &right[range.clone()]];
            let mut processor = PluginProcessor::new(instance.as_mut(), DEADLINE);
            let receipt = engine
                .process(
                    &ctx,
                    &InputBlock::new(&sp, &channels),
                    &mut processor,
                    &mut out,
                    &CancellationToken::default(),
                )
                .unwrap();
            let plugin_receipt = processor.last_receipt().unwrap().clone();
            assert_eq!(
                (
                    receipt.processed_frames,
                    receipt.latency_samples,
                    receipt.start_sample,
                    receipt.end_sample
                ),
                (256, 3, (block * 256) as u64, ((block + 1) * 256) as u64)
            );
            assert_eq!(
                (
                    plugin_receipt.frames,
                    plugin_receipt.latency_samples,
                    plugin_receipt.state_version
                ),
                (256, 3, 1)
            );
            assert_eq!(&*plugin_receipt.provider_identity, "reference-gain-delay@1");
            assert_eq!(plugin_receipt.completeness, Completeness::Complete);
            assert_eq!(plugin_receipt.instance_id, 1, "first instance of this host");
            assert_eq!(
                bits(out.published_channel(0).unwrap()),
                bits(&want_l[range.clone()]),
                "block {block} L"
            );
            assert_eq!(
                bits(out.published_channel(1).unwrap()),
                bits(&want_r[range]),
                "block {block} R"
            );
        }

        // Orderly retire: the worker leaves by itself (exit 0), is reaped, the permits come back after.
        let live = host.stats();
        assert_eq!((live.workers_live, live.spawned > 0), (1, true));
        assert!(live.in_flight_bytes > 0);
        let pid = host.ledger().last().unwrap().pid;
        assert!(pid_alive(pid), "worker must be alive before retire");
        let report = instance.retire();
        assert_eq!(
            (
                report.blocks_processed,
                report.killed,
                report.reaped,
                report.exit_code
            ),
            (2, false, true, Some(0))
        );
        assert!(report.released_bytes > 0);
        let after = host.stats();
        assert_eq!((after.workers_live, after.in_flight_bytes), (0, 0));
        assert!(!pid_alive(pid), "worker must be gone after retire");
        assert!(
            host.ledger()
                .iter()
                .all(|e| e.reaped && e.owner_session == "session-7")
        );
    });
}

#[test]
fn crash_and_timeout_are_contained() {
    bounded_case("crash_and_timeout_are_contained", || {
        let file = PluginFile::new();
        let cancel = CancellationToken::default();
        let (left, right) = signal(256);
        let sp = spec(256, 0);
        let channels: [&[f32]; 2] = [&left, &right];
        let input = InputBlock::new(&sp, &channels);

        // A worker that exits abnormally on its first block.
        let crashing = host(&["--fault", "crash-on-block=1"]);
        let mut instance = crashing
            .activate_isolated(&config(&file, 0.5), &cancel, DEADLINE)
            .unwrap();
        let pid = instance.pid().unwrap();
        let mut out = prefilled(256);
        let error = instance
            .process(&input, &mut out, &cancel, DEADLINE)
            .unwrap_err();
        assert_eq!(
            error,
            PluginError::Crashed {
                exit_code: Some(FAULT_EXIT_CODE)
            }
        );
        assert!(
            silent(&out, 256),
            "a crashed block leaves silence, never partial data"
        );
        let fault = instance.last_fault().unwrap().clone();
        assert_eq!(
            (fault.reaped, fault.safe_output, fault.exit_code),
            (true, true, Some(FAULT_EXIT_CODE))
        );
        assert!(!pid_alive(pid));
        assert_eq!(instance.lifecycle(), LifecycleState::Draining);
        // Only retire is legal after a fault.
        assert_eq!(
            instance
                .process(&input, &mut out, &cancel, DEADLINE)
                .unwrap_err(),
            PluginError::IllegalTransition {
                from: LifecycleState::Draining,
                to: LifecycleState::Processing
            }
        );
        let report = instance.retire_in_place();
        assert!(report.reaped);
        let s = crashing.stats();
        assert_eq!((s.crashed, s.workers_live, s.in_flight_bytes), (1, 0, 0));

        // A worker that hangs on its first block: killed at the deadline, reaped.
        let hanging = host(&["--fault", "hang-on-block=1"]);
        let mut instance = hanging
            .activate_isolated(&config(&file, 0.5), &cancel, DEADLINE)
            .unwrap();
        let pid = instance.pid().unwrap();
        let deadline = Duration::from_millis(300);
        let mut out = prefilled(256);
        let started = Instant::now();
        let error = instance
            .process(&input, &mut out, &cancel, deadline)
            .unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(error, PluginError::Timeout);
        assert!(
            elapsed >= deadline && elapsed < deadline * 2,
            "returned after {elapsed:?}"
        );
        assert!(silent(&out, 256));
        let fault = instance.last_fault().unwrap();
        assert_eq!(
            (fault.killed, fault.reaped, fault.safe_output),
            (true, true, true)
        );
        assert!(
            !pid_alive(pid),
            "a hung worker must be killed, not left running"
        );
        let report = instance.retire_in_place();
        assert!(report.killed && report.reaped);
        let s = hanging.stats();
        assert_eq!(
            (s.timed_out, s.killed, s.workers_live, s.in_flight_bytes),
            (1, 1, 0, 0)
        );
    });
}

#[test]
fn unauthorized_and_mismatch_rejected() {
    bounded_case("unauthorized_and_mismatch_rejected", || {
        let file = PluginFile::new();
        let host = host(&[]);
        let cancel = CancellationToken::default();
        let probe = |plugin: &ApprovedPlugin, grant: Option<&PluginGrant>| {
            host.probe(plugin, grant, &cancel, DEADLINE)
        };

        // Refusals before any process exists.
        assert_eq!(
            probe(&file.approved(), None).unwrap_err(),
            PluginError::Unauthorized
        );
        let other_grant = PluginGrant {
            actor: actor(),
            content_sha256: [9; 32],
        };
        assert_eq!(
            probe(&file.approved(), Some(&other_grant)).unwrap_err(),
            PluginError::Unauthorized
        );
        let mut wrong_hash = file.approved();
        wrong_hash.content_sha256 = [1; 32];
        let wrong_grant = PluginGrant {
            actor: actor(),
            content_sha256: [1; 32],
        };
        assert_eq!(
            probe(&wrong_hash, Some(&wrong_grant)).unwrap_err(),
            PluginError::HashMismatch
        );
        let mut other_arch = file.approved();
        other_arch.arch = if Arch::host() == Some(Arch::X86_64) {
            Arch::Aarch64
        } else {
            Arch::X86_64
        };
        assert_eq!(
            probe(&other_arch, Some(&grant())).unwrap_err(),
            PluginError::AbiMismatch
        );
        let mut other_abi = file.approved();
        other_abi.abi_version = 2;
        assert_eq!(
            probe(&other_abi, Some(&grant())).unwrap_err(),
            PluginError::AbiMismatch
        );
        let mut vst3 = file.approved();
        vst3.format = PluginFormat::Vst3;
        assert_eq!(
            probe(&vst3, Some(&grant())).unwrap_err(),
            PluginError::AbiMismatch
        );
        let mut missing = file.approved();
        missing.path = file.0.with_extension("missing");
        assert_eq!(
            probe(&missing, Some(&grant())).unwrap_err(),
            PluginError::Unavailable
        );
        assert_eq!(
            host.stats().spawned,
            0,
            "no worker may exist for a refused request"
        );
        assert!(host.ledger().is_empty());

        // The same request, authorized and matching, runs in a worker that is then reaped.
        probe(&file.approved(), Some(&grant())).unwrap();
        let s = host.stats();
        assert_eq!((s.spawned, s.reaped, s.workers_live), (1, 1, 0));

        // A worker that breaks the protocol is killed and reaped: garbage, oversize and short frames.
        let (left, right) = signal(256);
        let sp = spec(256, 0);
        let channels: [&[f32]; 2] = [&left, &right];
        for (fault, expected) in [
            ("garbage-on-block=1", ProtocolFault::Garbage),
            ("oversize-on-block=1", ProtocolFault::Oversize),
            ("short-on-block=1", ProtocolFault::Truncated),
        ] {
            let bad = self::host(&["--fault", fault]);
            let mut instance = bad
                .activate_isolated(&config(&file, 0.5), &cancel, DEADLINE)
                .unwrap();
            let pid = instance.pid().unwrap();
            let mut out = prefilled(256);
            let error = instance
                .process(
                    &InputBlock::new(&sp, &channels),
                    &mut out,
                    &cancel,
                    DEADLINE,
                )
                .unwrap_err();
            assert_eq!(error, PluginError::Protocol(expected), "{fault}");
            assert!(silent(&out, 256), "{fault}: silence on protocol fault");
            assert!(instance.last_fault().unwrap().reaped, "{fault}");
            assert!(!pid_alive(pid), "{fault}: worker must be gone");
            instance.retire_in_place();
            let s = bad.stats();
            assert_eq!(
                (s.protocol_faults, s.workers_live, s.in_flight_bytes),
                (1, 0, 0),
                "{fault}"
            );
        }

        // A worker that never answers the probe: ProbeTimeout, killed and reaped.
        let silent_host = self::host(&["--fault", "hang-on-probe"]);
        let started = Instant::now();
        let error = silent_host
            .probe(
                &file.approved(),
                Some(&grant()),
                &cancel,
                Duration::from_millis(300),
            )
            .unwrap_err();
        assert_eq!(error, PluginError::ProbeTimeout);
        assert!(started.elapsed() < Duration::from_secs(5));
        let s = silent_host.stats();
        assert_eq!((s.killed, s.reaped, s.workers_live), (1, 1, 0));
    });
}

#[test]
fn lifecycle_and_stale_state() {
    bounded_case("lifecycle_and_stale_state", || {
        // The lifecycle table: the legal chain, and the calls that must be illegal.
        let chain = [
            LifecycleState::Discovered,
            LifecycleState::Probed,
            LifecycleState::Active,
            LifecycleState::Processing,
            LifecycleState::Active,
            LifecycleState::Draining,
            LifecycleState::Retired,
        ];
        for pair in chain.windows(2) {
            assert_eq!(pair[0].transition(pair[1]), Ok(pair[1]));
        }
        assert_eq!(
            LifecycleState::Discovered.transition(LifecycleState::Processing),
            Err(PluginError::IllegalTransition {
                from: LifecycleState::Discovered,
                to: LifecycleState::Processing
            })
        );
        assert!(
            LifecycleState::Retired
                .transition(LifecycleState::Active)
                .is_err()
        );
        assert!(
            LifecycleState::Retired
                .transition(LifecycleState::Retired)
                .is_err()
        );

        let file = PluginFile::new();
        let cancel = CancellationToken::default();
        let (left, right) = signal(256);
        let sp = spec(256, 0);
        let channels: [&[f32]; 2] = [&left, &right];
        let input = InputBlock::new(&sp, &channels);

        let host = host(&[]);
        let mut instance = host
            .activate_isolated(&config(&file, 0.5), &cancel, DEADLINE)
            .unwrap();
        // Versioned, complete state capture: version 1, state = gain 0.5 as f64.
        let capture = instance.capture_state(&cancel, DEADLINE, 64).unwrap();
        assert_eq!(
            (capture.state_version, capture.completeness),
            (1, Completeness::Complete)
        );
        assert_eq!(capture.bytes, 0.5_f64.to_le_bytes().to_vec());
        // A capture larger than the worker's blob budget is refused by the worker, instance stays usable.
        assert_eq!(
            instance.capture_state(&cancel, DEADLINE, 4).unwrap_err(),
            PluginError::BudgetExceeded
        );
        assert_eq!(
            instance.capture_state(&cancel, DEADLINE, 65).unwrap_err(),
            PluginError::BudgetExceeded
        );
        assert_eq!(instance.lifecycle(), LifecycleState::Active);

        // The state changes (version 2); a block prepared under version 1 is refused, nothing written.
        assert_eq!(
            instance.set_parameter(Parameter { id: 0, value: 0.25 }, &cancel, DEADLINE),
            Ok(2)
        );
        let mut out = prefilled(256);
        assert_eq!(
            instance
                .process_expecting(1, &input, &mut out, &cancel, DEADLINE)
                .unwrap_err(),
            PluginError::StaleState
        );
        assert!(
            out.backing().iter().all(|s| *s == 7.0),
            "a refused block must not touch the output"
        );
        let receipt = instance
            .process_expecting(2, &input, &mut out, &cancel, DEADLINE)
            .unwrap();
        assert_eq!((receipt.state_version, receipt.frames), (2, 256));
        let want: Vec<f32> = left.iter().map(|s| s * 0.25).collect();
        assert_eq!(bits(&out.backing()[..256]), bits(&want));
        assert_eq!(
            instance.capture_state(&cancel, DEADLINE, 64).unwrap().bytes,
            0.25_f64.to_le_bytes().to_vec()
        );
        // Invalid parameter values are typed refusals; the worker keeps running.
        assert_eq!(
            instance.set_parameter(
                Parameter {
                    id: 0,
                    value: f64::NAN
                },
                &cancel,
                DEADLINE
            ),
            Err(PluginError::InvalidConfig)
        );
        assert_eq!(
            instance.set_parameter(Parameter { id: 9, value: 1.0 }, &cancel, DEADLINE),
            Err(PluginError::InvalidConfig)
        );
        assert_eq!(instance.lifecycle(), LifecycleState::Active);

        // Permits are still held; retire releases them only after the reap, and the instance is then dead.
        assert_eq!(
            (host.stats().workers_live, host.stats().in_flight_bytes > 0),
            (1, true)
        );
        let report = instance.retire_in_place();
        assert!(report.reaped);
        assert_eq!(
            (host.stats().workers_live, host.stats().in_flight_bytes),
            (0, 0)
        );
        let illegal = PluginError::IllegalTransition {
            from: LifecycleState::Retired,
            to: LifecycleState::Processing,
        };
        assert_eq!(
            instance
                .process(&input, &mut out, &cancel, DEADLINE)
                .unwrap_err(),
            illegal
        );
        assert_eq!(
            instance.capture_state(&cancel, DEADLINE, 64).unwrap_err(),
            illegal
        );
        assert_eq!(instance.retire_in_place(), report, "retire is idempotent");

        // Partial and unavailable state are reported as such, never as complete.
        for (fault, completeness, len) in [
            ("partial-state", Completeness::Partial, 4),
            ("no-state", Completeness::Unavailable, 0),
        ] {
            let host = self::host(&["--fault", fault]);
            let mut instance = host
                .activate_isolated(&config(&file, 0.5), &cancel, DEADLINE)
                .unwrap();
            let capture = instance.capture_state(&cancel, DEADLINE, 64).unwrap();
            assert_eq!(
                (capture.completeness, capture.bytes.len()),
                (completeness, len),
                "{fault}"
            );
            let mut out = prefilled(256);
            let receipt = instance
                .process(&input, &mut out, &cancel, DEADLINE)
                .unwrap();
            assert_eq!(receipt.completeness, completeness, "{fault}");
            instance.retire_in_place();
        }
    });
}

struct VecSink(Vec<DeliveryClass>);
impl SinkPort for VecSink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        assert!(bytes.len() <= MAX_FRAME_BYTES);
        self.0.push(class);
        Ok(())
    }
}

#[test]
fn admission_and_cancel() {
    bounded_case("admission_and_cancel", || {
        let file = PluginFile::new();
        let cancel = CancellationToken::default();

        // max_workers = 1: the second activation is refused at once, without spawning or waiting.
        let limits = HostLimits {
            max_workers: 1,
            ..HostLimits::default()
        };
        let one = host_with(&[], limits);
        let first = one
            .activate_isolated(&config(&file, 0.5), &cancel, DEADLINE)
            .unwrap();
        let started = Instant::now();
        let refused = one
            .activate_isolated(&config(&file, 0.5), &cancel, DEADLINE)
            .unwrap_err();
        assert_eq!(refused, PluginError::BudgetExceeded);
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "admission must not wait"
        );
        assert_eq!(one.stats().spawned, 1);
        assert_eq!(
            one.probe(&file.approved(), Some(&grant()), &cancel, DEADLINE)
                .unwrap_err(),
            PluginError::BudgetExceeded
        );
        // Retire frees the slot: the next activation is admitted again.
        let mut first = first;
        first.retire_in_place();
        let mut again = one
            .activate_isolated(&config(&file, 0.5), &cancel, DEADLINE)
            .unwrap();
        again.retire_in_place();
        assert_eq!(
            (one.stats().workers_live, one.stats().in_flight_bytes),
            (0, 0)
        );

        // A byte budget below one instance's buffers is refused and the worker slot is given back.
        let tiny = HostLimits {
            max_in_flight_bytes: 1024,
            ..HostLimits::default()
        };
        let small = host_with(&[], tiny);
        assert_eq!(
            small
                .activate_isolated(&config(&file, 0.5), &cancel, DEADLINE)
                .unwrap_err(),
            PluginError::BudgetExceeded
        );
        assert_eq!((small.stats().workers_live, small.stats().spawned), (0, 0));

        // A pre-cancelled token spawns nothing.
        let canceled = CancellationToken::default();
        canceled.cancel();
        assert_eq!(
            one.activate_isolated(&config(&file, 0.5), &canceled, DEADLINE)
                .unwrap_err(),
            PluginError::Canceled
        );
        assert_eq!(one.stats().spawned, 2);

        // Cancel mid-process: the hung worker is killed and reaped, the engine publishes nothing.
        let hanging = host(&["--fault", "hang-on-block=1"]);
        let mut instance = hanging
            .activate_isolated(&config(&file, 0.5), &cancel, DEADLINE)
            .unwrap();
        let pid = instance.pid().unwrap();
        let (left, right) = signal(256);
        let sp = spec(256, 0);
        let channels: [&[f32]; 2] = [&left, &right];
        let resource = DomainId::parse(RESOURCE).unwrap();
        let engine = Engine::new(resource.clone(), 7);
        let ctx = RequestContext {
            resource: resource.clone(),
            actor: Some(actor()),
            expected_revision: 7,
        };
        let mut out = OutputBlock::with_capacity(ChannelLayout::stereo(), 256).unwrap();
        let mid = CancellationToken::default();
        let trigger = mid.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            trigger.cancel();
        });
        let started = Instant::now();
        let (result, typed) = {
            let mut processor = PluginProcessor::new(&mut instance, Duration::from_secs(30));
            let result = engine.process(
                &ctx,
                &InputBlock::new(&sp, &channels),
                &mut processor,
                &mut out,
                &mid,
            );
            (result, processor.take_error())
        };
        canceller.join().unwrap();
        assert_eq!(result.unwrap_err(), ScoreError::Canceled);
        assert_eq!(typed, Some(PluginError::Canceled));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "cancel must not wait for the deadline"
        );
        assert_eq!(out.published_frames(), 0, "no partial output is published");
        let fault = instance.last_fault().unwrap();
        assert_eq!(
            (fault.killed, fault.reaped, fault.error),
            (true, true, PluginError::Canceled)
        );
        assert!(!pid_alive(pid));
        instance.retire_in_place();
        let s = hanging.stats();
        assert_eq!((s.canceled, s.workers_live, s.in_flight_bytes), (1, 0, 0));

        // One terminal frame per outcome, then the emitter is closed (never a duplicate).
        let results: [(Result<(), PluginError>, Outcome); 5] = [
            (Ok(()), Outcome::Success),
            (Err(PluginError::Canceled), Outcome::Canceled),
            (
                Err(PluginError::HashMismatch),
                Outcome::Failure(FailureCode::Validation),
            ),
            (
                Err(PluginError::Crashed {
                    exit_code: Some(86),
                }),
                Outcome::Failure(FailureCode::Unavailable),
            ),
            (
                Err(PluginError::StaleState),
                Outcome::Failure(FailureCode::Loss),
            ),
        ];
        for (result, outcome) in &results {
            let mut observe = Observe::new(
                9,
                7,
                DomainId::parse(RESOURCE).unwrap(),
                actor(),
                Budget::new(0, 0).unwrap(),
            );
            let mut sink = VecSink(Vec::new());
            let receipt = report(result, 9, 7, &cancel, &mut observe, &mut sink).unwrap();
            assert_eq!((receipt.outcome, outcome_of(result)), (*outcome, *outcome));
            assert_eq!(sink.0, vec![DeliveryClass::Terminal]);
            assert_eq!(
                report(result, 9, 7, &cancel, &mut observe, &mut sink).unwrap_err(),
                ObserveError::Closed
            );
            assert_eq!(sink.0.len(), 1);
        }
    });
}
