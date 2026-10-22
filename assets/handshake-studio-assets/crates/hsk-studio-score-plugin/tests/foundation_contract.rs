//! Process-isolation contract of the Score Plugin provider, default features (no fault injection),
//! proven with the REFERENCE worker (a pure Rust gain-and-delay processor behind the real protocol).
//! This proves spawn, reap, authorization, lifecycle, state versioning and budgets; it does NOT prove
//! CLAP or VST3 (MT-35560: a mock host does not prove a plugin format). Crash, hang, protocol-fault and
//! mid-call cancel behaviour is proven in `fault_injection.rs` (feature `test-faults`); the native CLAP
//! path in `clap_fixture.rs` (feature `clap`). Shared helpers and oracles: `common/mod.rs`.
mod common;

use common::*;
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, Error as ObserveError, FailureCode, MAX_FRAME_BYTES,
    Observe, Outcome, SinkPort,
};
use hsk_studio_score::{
    BlockSpec, ChannelLayout, Engine, InputBlock, OutputBlock, RequestContext, sha256,
};
use hsk_studio_score_plugin::*;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[test]
fn worker_gain_block_exact() {
    bounded_case("worker_gain_block_exact", || {
        let file = PluginFile::new();
        let host = host_with(HostLimits::default(), 3);
        let cancel = CancellationToken::default();
        let descriptor = host
            .probe(&file.approved(), Some(&grant()), &cancel, DEADLINE)
            .unwrap();
        assert_eq!(
            (descriptor.format, descriptor.abi_version, descriptor.inputs),
            (PluginFormat::Clap, 1, 2)
        );
        assert_eq!(descriptor.provider_identity, REFERENCE_IDENTITY);
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
            assert_eq!(&*plugin_receipt.provider_identity, REFERENCE_IDENTITY);
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
fn unauthorized_and_mismatch_rejected() {
    bounded_case("unauthorized_and_mismatch_rejected", || {
        let file = PluginFile::new();
        let host = host();
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

        // A worker program that does not exist is `unavailable`, never a fallback.
        let mut missing_worker = options(HostLimits::default(), 0);
        missing_worker.program = Some(file.0.with_extension("no-worker"));
        assert_eq!(
            IsolatedHost::new(missing_worker).err(),
            Some(PluginError::Unavailable)
        );

        // V4-04: a default build has no fault hook; the worker rejects the flag as unknown (exit 2)
        // before it reads a single request.
        #[cfg(not(feature = "test-faults"))]
        {
            let mut command = std::process::Command::new(WORKER);
            command
                .args(["--fault", "crash-on-block=1"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000);
            }
            assert_eq!(command.status().unwrap().code(), Some(2));
        }
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

        let host = host();
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
        let one = host_with(limits, 0);
        let mut first = one
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
        let small = host_with(tiny, 0);
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

#[test]
fn host_limits_are_bounded_at_admission() {
    bounded_case("host_limits_are_bounded_at_admission", || {
        // R6: spawn_timeout and shutdown_grace were only checked non-zero, so Duration::MAX reached
        // `Instant::now() + limit` in the supervisor and panicked inside the host. Now every duration
        // limit is 1 ms ..= MAX_DEADLINE at admission, with a typed error.
        let base = HostLimits::default();
        let refused = |limits: HostLimits| IsolatedHost::new(options(limits, 0)).err();
        let one_over = MAX_DEADLINE + Duration::from_millis(1);
        for (name, limits) in [
            (
                "spawn_timeout MAX",
                HostLimits {
                    spawn_timeout: Duration::MAX,
                    ..base.clone()
                },
            ),
            (
                "spawn_timeout over",
                HostLimits {
                    spawn_timeout: one_over,
                    ..base.clone()
                },
            ),
            (
                "spawn_timeout zero",
                HostLimits {
                    spawn_timeout: Duration::ZERO,
                    ..base.clone()
                },
            ),
            (
                "shutdown_grace MAX",
                HostLimits {
                    shutdown_grace: Duration::MAX,
                    ..base.clone()
                },
            ),
            (
                "shutdown_grace over",
                HostLimits {
                    shutdown_grace: one_over,
                    ..base.clone()
                },
            ),
            (
                "shutdown_grace zero",
                HostLimits {
                    shutdown_grace: Duration::ZERO,
                    ..base.clone()
                },
            ),
        ] {
            assert_eq!(refused(limits), Some(PluginError::InvalidConfig), "{name}");
        }

        // The largest accepted values work end to end (spawn, handshake, probe, shutdown, reap).
        let widest = HostLimits {
            spawn_timeout: MAX_DEADLINE,
            shutdown_grace: MAX_DEADLINE,
            ..base
        };
        let host = host_with(widest, 0);
        let file = PluginFile::new();
        let cancel = CancellationToken::default();
        host.probe(&file.approved(), Some(&grant()), &cancel, MAX_DEADLINE)
            .unwrap();
        let s = host.stats();
        assert_eq!((s.spawned, s.reaped, s.workers_live), (1, 1, 0));

        // A per-call deadline of Duration::MAX is refused before any process exists.
        for deadline in [
            Duration::MAX,
            MAX_DEADLINE + Duration::from_millis(1),
            Duration::ZERO,
        ] {
            assert_eq!(
                host.probe(&file.approved(), Some(&grant()), &cancel, deadline)
                    .unwrap_err(),
                PluginError::InvalidConfig
            );
            assert_eq!(
                host.activate_isolated(&config(&file, 0.5), &cancel, deadline)
                    .unwrap_err(),
                PluginError::InvalidConfig
            );
        }
        let mut instance = host
            .activate_isolated(&config(&file, 0.5), &cancel, DEADLINE)
            .unwrap();
        let (left, right) = signal(256);
        let sp = spec(256, 0);
        let channels: [&[f32]; 2] = [&left, &right];
        let input = InputBlock::new(&sp, &channels);
        let mut out = prefilled(256);
        assert_eq!(
            instance
                .process(&input, &mut out, &cancel, Duration::MAX)
                .unwrap_err(),
            PluginError::InvalidConfig
        );
        assert!(
            out.backing().iter().all(|s| *s == 7.0),
            "a refused call must not touch the output"
        );
        assert_eq!(
            instance
                .set_parameter(Parameter { id: 0, value: 0.25 }, &cancel, Duration::MAX)
                .unwrap_err(),
            PluginError::InvalidConfig
        );
        assert_eq!(
            instance
                .capture_state(&cancel, Duration::MAX, 64)
                .unwrap_err(),
            PluginError::InvalidConfig
        );
        assert_eq!(
            instance.lifecycle(),
            LifecycleState::Active,
            "refusals leave the worker running"
        );
        assert!(instance.retire_in_place().reaped);
        assert_eq!(
            host.stats().spawned,
            2,
            "only the probe and this one activation spawned"
        );
    });
}

#[test]
fn plugin_file_bound_applies_and_its_size_is_not_charged() {
    bounded_case(
        "plugin_file_bound_applies_and_its_size_is_not_charged",
        || {
            // The approved file is streamed over one fixed buffer (tests/hash_memory.rs proves the memory), so
            // its size is neither buffered nor charged to the admission budget: a 1000-byte and a 3-byte
            // plugin reserve exactly the same. The per-file bound still applies, before any process exists.
            let bytes = vec![0x5A_u8; 1000];
            let sha = sha256(&bytes);
            let big = PluginFile::with_bytes(&bytes);
            let small = PluginFile::new();
            let cancel = CancellationToken::default();
            let big_config = ProcessConfig {
                plugin: big.approved_with(sha),
                grant: Some(PluginGrant {
                    actor: actor(),
                    content_sha256: sha,
                }),
                sample_rate_hz: 48_000,
                layout: ChannelLayout::stereo(),
                max_block_frames: 256,
                parameters: Vec::new(),
                max_state_bytes: 64,
            };
            let host = host();
            let mut a = host
                .activate_isolated(&big_config, &cancel, DEADLINE)
                .unwrap();
            let with_big = host.stats().in_flight_bytes;
            a.retire_in_place();
            let mut b = host
                .activate_isolated(&config(&small, 0.5), &cancel, DEADLINE)
                .unwrap();
            assert_eq!(
                host.stats().in_flight_bytes,
                with_big,
                "the file size is not part of the reservation"
            );
            b.retire_in_place();
            assert_eq!(host.stats().in_flight_bytes, 0);

            // The bound: one byte under the file is refused as budget_exceeded with no process; at the file
            // size the same plugin is accepted.
            for (limit, ok) in [(999_u64, false), (1000, true)] {
                let host = host_with(
                    HostLimits {
                        max_plugin_file_bytes: limit,
                        ..HostLimits::default()
                    },
                    0,
                );
                let result = host.probe(
                    &big.approved_with(sha),
                    Some(&PluginGrant {
                        actor: actor(),
                        content_sha256: sha,
                    }),
                    &cancel,
                    DEADLINE,
                );
                assert_eq!(result.is_ok(), ok, "limit {limit}: {result:?}");
                if !ok {
                    assert_eq!(result.unwrap_err(), PluginError::BudgetExceeded);
                    assert_eq!(host.stats().spawned, 0);
                }
            }
        },
    );
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    found.sort();
    found
}

struct RemoveDir(PathBuf);
impl Drop for RemoveDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn staging_directories_follow_the_worker() {
    bounded_case("staging_directories_follow_the_worker", || {
        let root = RemoveDir(std::env::temp_dir().join(format!(
            "hsk-score-plugin-stage-root-{}",
            std::process::id()
        )));
        std::fs::create_dir_all(&root.0).unwrap();
        let file = PluginFile::new();
        let cancel = CancellationToken::default();

        // The reference backend never loads the file, so the host makes no staging directory.
        let mut reference = options(HostLimits::default(), 0);
        reference.staging_root = Some(root.0.clone());
        let reference_host = IsolatedHost::new(reference).unwrap();
        reference_host
            .probe(&file.approved(), Some(&grant()), &cancel, DEADLINE)
            .unwrap();
        assert!(
            entries(&root.0).is_empty(),
            "no staging directory for the reference backend"
        );
        drop(reference_host);

        // The CLAP backend gets one private directory per host and one subdirectory per worker; the
        // plugin "abc" cannot be served (a default-feature worker has no CLAP backend and exits;
        // a clap worker finds no library in the verified copy), so the probe is unavailable.
        let mut clap = options(HostLimits::default(), 0);
        clap.backend = WorkerBackend::Clap;
        clap.staging_root = Some(root.0.clone());
        let host = IsolatedHost::new(clap).unwrap();
        let host_dirs = entries(&root.0);
        assert_eq!(host_dirs.len(), 1, "{host_dirs:?}");
        assert!(entries(&host_dirs[0]).is_empty());
        assert_eq!(
            host.probe(&file.approved(), Some(&grant()), &cancel, DEADLINE)
                .unwrap_err(),
            PluginError::Unavailable
        );
        let s = host.stats();
        assert_eq!((s.spawned, s.reaped, s.workers_live), (1, 1, 0));
        assert!(
            entries(&host_dirs[0]).is_empty(),
            "the worker directory is removed once the worker is reaped: {:?}",
            entries(&host_dirs[0])
        );
        drop(host);
        assert!(
            !host_dirs[0].exists(),
            "the host directory goes with the host"
        );
        assert!(entries(&root.0).is_empty());
    });
}

#[test]
fn sample_rate_is_refused_at_activation_before_any_spawn() {
    bounded_case(
        "sample_rate_is_refused_at_activation_before_any_spawn",
        || {
            // PLR-054: the host once applied only `0 < rate <= 768_000`, so a rate that is not an exact
            // divisor of the tick rate (44_101, 47_999, ...) activated a worker and failed at the first
            // block. The rule is score's `ticks_per_sample`; a refusal is `invalid_config` before any
            // process exists, through both `activate` and `activate_isolated`.
            let file = PluginFile::new();
            let host = host();
            let cancel = CancellationToken::default();
            let config_at = |rate: u32| ProcessConfig {
                sample_rate_hz: rate,
                ..config(&file, 0.5)
            };
            for rate in [
                0,
                44_101,
                47_999,
                22_051,
                96_001,
                11_000,
                768_001,
                1_536_000,
                u32::MAX,
            ] {
                assert_eq!(
                    host.activate_isolated(&config_at(rate), &cancel, DEADLINE)
                        .unwrap_err(),
                    PluginError::InvalidConfig,
                    "activate_isolated at {rate} Hz"
                );
                assert_eq!(
                    host.activate(&config_at(rate), &cancel, DEADLINE).err(),
                    Some(PluginError::InvalidConfig),
                    "activate at {rate} Hz"
                );
            }
            let s = host.stats();
            assert_eq!(
                (s.spawned, s.workers_live, s.in_flight_bytes),
                (0, 0, 0),
                "a refused rate must not spawn a worker or hold a permit"
            );

            // The accepted side: rates the score rule admits activate (and retire) normally.
            for rate in [8_000, 44_100, 48_000, 500_000, 705_600, 768_000] {
                let mut instance = host
                    .activate_isolated(&config_at(rate), &cancel, DEADLINE)
                    .unwrap_or_else(|e| panic!("{rate} Hz refused: {e}"));
                assert_eq!(instance.sample_rate_hz(), rate);
                assert!(instance.retire_in_place().reaped, "{rate} Hz");
            }
            assert_eq!(host.stats().spawned, 6);

            // One block at an accepted non-default rate runs end to end: the configured rate is the
            // block's rate, so nothing is left to fail at the first block.
            let mut instance = host
                .activate_isolated(&config_at(44_100), &cancel, DEADLINE)
                .unwrap();
            let (left, right) = signal(256);
            let sp = BlockSpec {
                sample_rate_hz: 44_100,
                ..spec(256, 0)
            };
            let channels: [&[f32]; 2] = [&left, &right];
            let input = InputBlock::new(&sp, &channels);
            let mut out = prefilled(256);
            let receipt = instance
                .process(&input, &mut out, &cancel, DEADLINE)
                .unwrap();
            assert_eq!(receipt.frames, 256);
            let want: Vec<f32> = left.iter().map(|s| s * 0.5).collect();
            assert_eq!(bits(&out.backing()[..256]), bits(&want));
            assert!(instance.retire_in_place().reaped);
        },
    );
}
