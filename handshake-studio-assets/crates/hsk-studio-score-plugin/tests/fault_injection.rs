#![cfg(feature = "test-faults")]
//! Fault containment of the Score Plugin provider (cargo feature `test-faults`, NOT a default feature:
//! the worker's fault modes are not compiled into a normal build). The REFERENCE worker crashes, hangs,
//! breaks the protocol or reports partial state on request, and the host must contain every one of
//! them: typed error, silence in the output block, worker killed or exited AND reaped (read from the
//! operating system), permits back to zero. Shared helpers and oracles: `common/mod.rs`.
mod common;

use common::*;
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_score::{
    ChannelLayout, Engine, InputBlock, OutputBlock, RequestContext, ScoreError,
};
use hsk_studio_score_plugin::*;
use std::time::{Duration, Instant};

fn fault_host(faults: &[&str]) -> IsolatedHost {
    let mut options = options(HostLimits::default(), 0);
    for fault in faults {
        options
            .test_args
            .extend(["--fault".to_owned(), (*fault).to_owned()]);
    }
    IsolatedHost::new(options).unwrap()
}

#[test]
fn process_faults_are_contained() {
    bounded_case("process_faults_are_contained", || {
        let file = PluginFile::new();
        let cancel = CancellationToken::default();
        let (left, right) = signal(256);
        let sp = spec(256, 0);
        let channels: [&[f32]; 2] = [&left, &right];
        let input = InputBlock::new(&sp, &channels);

        // A worker that exits abnormally on its first block.
        let crashing = fault_host(&["crash-on-block=1"]);
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
        let hanging = fault_host(&["hang-on-block=1"]);
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

        // A worker that breaks the protocol is killed and reaped: garbage, oversize and short frames.
        for (fault, expected) in [
            ("garbage-on-block=1", ProtocolFault::Garbage),
            ("oversize-on-block=1", ProtocolFault::Oversize),
            ("short-on-block=1", ProtocolFault::Truncated),
        ] {
            let bad = fault_host(&[fault]);
            let mut instance = bad
                .activate_isolated(&config(&file, 0.5), &cancel, DEADLINE)
                .unwrap();
            let pid = instance.pid().unwrap();
            let mut out = prefilled(256);
            let error = instance
                .process(&input, &mut out, &cancel, DEADLINE)
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
        let silent_host = fault_host(&["hang-on-probe"]);
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
fn state_completeness_and_cancel_mid_process() {
    bounded_case("state_completeness_and_cancel_mid_process", || {
        let file = PluginFile::new();
        let cancel = CancellationToken::default();
        let (left, right) = signal(256);
        let sp = spec(256, 0);
        let channels: [&[f32]; 2] = [&left, &right];
        let input = InputBlock::new(&sp, &channels);

        // Partial and unavailable state are reported as such, never as complete.
        for (fault, completeness, len) in [
            ("partial-state", Completeness::Partial, 4),
            ("no-state", Completeness::Unavailable, 0),
        ] {
            let host = fault_host(&[fault]);
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

        // Cancel mid-process: the hung worker is killed and reaped, the engine publishes nothing.
        let hanging = fault_host(&["hang-on-block=1"]);
        let mut instance = hanging
            .activate_isolated(&config(&file, 0.5), &cancel, DEADLINE)
            .unwrap();
        let pid = instance.pid().unwrap();
        let resource = DomainId::parse(RESOURCE).unwrap();
        let engine = Engine::new(resource.clone(), 7);
        let ctx = RequestContext {
            resource,
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
            let result = engine.process(&ctx, &input, &mut processor, &mut out, &mid);
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
    });
}

#[test]
fn worker_cannot_rewrite_the_state_version() {
    bounded_case("worker_cannot_rewrite_the_state_version", || {
        // R6 trust boundary: the host used to adopt the version a worker reported (E_STALE detail,
        // ParamSet.version, State.version), so a misbehaving plugin process could rewind
        // `state_version` and have a block prepared under older state accepted. The host owns the
        // counter now: ParamSet must report exactly host + 1, a capture must report the host version,
        // and a worker that calls the host's own version stale is inconsistent. In every case the
        // worker is reclaimed, the host version is untouched and nothing is published.
        let file = PluginFile::new();
        let cancel = CancellationToken::default();
        let (left, right) = signal(256);
        let sp = spec(256, 0);
        let channels: [&[f32]; 2] = [&left, &right];
        let input = InputBlock::new(&sp, &channels);

        // Control: an honest worker is accepted by every one of the new checks.
        let honest = fault_host(&[]);
        let mut instance = honest
            .activate_isolated(&config(&file, 0.5), &cancel, DEADLINE)
            .unwrap();
        assert_eq!(
            instance.set_parameter(Parameter { id: 0, value: 0.25 }, &cancel, DEADLINE),
            Ok(2)
        );
        assert_eq!(
            instance
                .capture_state(&cancel, DEADLINE, 64)
                .unwrap()
                .state_version,
            2
        );
        let mut out = prefilled(256);
        assert_eq!(
            instance
                .process_expecting(2, &input, &mut out, &cancel, DEADLINE)
                .unwrap()
                .state_version,
            2
        );
        instance.retire_in_place();

        for (fault, call) in [
            ("lie-param-version", "set_parameter"),
            ("lie-capture-version", "capture_state"),
            ("stale-on-block=1", "process"),
        ] {
            let host = fault_host(&[fault]);
            let mut instance = host
                .activate_isolated(&config(&file, 0.5), &cancel, DEADLINE)
                .unwrap();
            let pid = instance.pid().unwrap();
            assert_eq!(instance.state_version(), 1);
            let mut out = prefilled(256);
            let error = match call {
                "set_parameter" => instance
                    .set_parameter(Parameter { id: 0, value: 0.25 }, &cancel, DEADLINE)
                    .unwrap_err(),
                "capture_state" => instance.capture_state(&cancel, DEADLINE, 64).unwrap_err(),
                _ => instance
                    .process_expecting(1, &input, &mut out, &cancel, DEADLINE)
                    .unwrap_err(),
            };
            assert_eq!(
                error,
                PluginError::Protocol(ProtocolFault::UnexpectedKind),
                "{fault}: a dishonest version is a protocol fault, not an adopted number"
            );
            assert_eq!(
                instance.state_version(),
                1,
                "{fault}: the host counter is not rewritten"
            );
            assert_eq!(instance.lifecycle(), LifecycleState::Draining, "{fault}");
            let report = instance.last_fault().unwrap();
            assert_eq!(report.error, error, "{fault}");
            assert!(report.reaped, "{fault}");
            assert!(!pid_alive(pid), "{fault}: the worker must be gone");
            if call == "process" {
                assert!(
                    silent(&out, 256),
                    "{fault}: nothing from the worker is published"
                );
            }
            instance.retire_in_place();
            let s = host.stats();
            assert_eq!(
                (s.protocol_faults, s.workers_live, s.in_flight_bytes),
                (1, 0, 0),
                "{fault}"
            );
        }
    });
}
