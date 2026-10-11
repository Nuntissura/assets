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
use hsk_studio_score::{ChannelLayout, Engine, InputBlock, OutputBlock, RequestContext};
use hsk_studio_score_plugin::*;
use std::time::{Duration, Instant};

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
