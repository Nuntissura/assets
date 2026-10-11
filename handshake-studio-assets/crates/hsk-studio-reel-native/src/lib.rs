//! Optional native media codec provider; isolate FFI/toolchain/codec license/version/process
//! boundaries from pure media contracts.
//!
//! Default build: empty and pure (`forbid(unsafe_code)`, no dependencies). Features:
//! * `av1` (host side): `WorkerAv1Provider` is reel's `ExternalVideoDecoder`: register it with
//!   `Registry::with_video_decoder(Arc::new(provider))` and every UNTRUSTED `isobmff`/`matroska` decode
//!   request for AV1 reaches the worker (reel's route does the container work and the receipts). It also
//!   is a `MediaProvider` (id `av1`, `decode_av1` for the typed `Av1Error`): the same path on an
//!   untrusted route whose only decoder is the worker. The codec configuration and temporal units travel
//!   over a versioned length-prefixed stdin/stdout protocol. This feature uses reel WITHOUT its `av1`
//!   feature, so a host built with it does not link the rav1d decoder; it checks the decoder id and
//!   learns the decoder version from the worker's HELLO (`cargo tree -e features` shows no rav1d).
//! * `av1-worker`: the `reel-native-av1-worker` binary (`worker_main`), which hosts reel's in-process
//!   rav1d decoder (`hsk_studio_reel::av1::Av1Decoder`, reel feature `av1`) in its own process. Build the
//!   worker executable with this feature; a host that only enables `av1` never links the decoder.
//! * `test-faults` (tests only, never in product builds): worker fault injection (`WorkerTestFault`,
//!   `Av1WorkerOptions::test_fault`); without it a worker ignores the fault environment variable.
//!
//! Reason (reel lane SMB-REEL-ENG-3): rav1d 1.1.0 aborts the whole process on a few percent of corrupted
//! bitstreams (an uncatchable `unwrap` inside an `extern "C"` function), so untrusted AV1 must not be
//! decoded in the caller's process. A worker crash, hang, protocol violation or oversize reply becomes a
//! typed `Av1WorkerFault` (`decoder_crashed`, `decoder_deadline`, ...; reel's `ReelError::DecoderFault` for
//! crash, deadline and protocol faults) and the provider stays usable with bounded respawn.
#![forbid(unsafe_code)]

#[cfg(feature = "av1")]
mod host;
#[cfg(all(feature = "av1", feature = "test-faults"))]
mod test_fault;
#[cfg(feature = "av1")]
mod wire;
#[cfg(feature = "av1-worker")]
mod worker;

#[cfg(feature = "av1")]
pub use host::{
    AV1_PROVIDER_ID, Av1ConfigError, Av1Error, Av1WorkerFault, Av1WorkerOptions, Av1WorkerStats,
    WORKER_PROGRAM, WorkerAv1Provider,
};
#[cfg(all(feature = "av1", feature = "test-faults"))]
pub use test_fault::WorkerTestFault;
#[cfg(feature = "av1-worker")]
pub use worker::{PANIC_EXIT_CODE, worker_main};

pub const DESCRIPTOR: &str = r#"{"crate":"hsk-studio-reel-native","module":"reel_native","default_closure":"empty, pure Rust, forbid(unsafe_code), no dependencies","features":{"av1":"optional, default off, host side: WorkerAv1Provider (reel ExternalVideoDecoder via Registry::with_video_decoder, and provider id av1) driving reel-native-av1-worker processes; uses hsk-studio-reel without its av1 feature, so the host does not link rav1d; decoder id checked and decoder version learned from the worker HELLO (version reported as unknown until the first worker completed HELLO)","av1-worker":"optional, default off: the reel-native-av1-worker binary hosting rav1d via hsk-studio-reel feature av1; build the worker executable with it","test-faults":"tests only, never in product builds: worker fault injection (WorkerTestFault, Av1WorkerOptions::test_fault); without it the worker does not read HSK_REEL_NATIVE_AV1_WORKER_TEST_FAULT (test worker_fault_env)"},"isolation":{"model":"one request per packet over versioned length-prefixed stdin/stdout, host owns and reaps the child, no in-process fallback","spawn":"CREATE_NO_WINDOW on Windows, empty environment, null stderr","bounds":["max_workers","bounded respawn per window","per-request batch deadline","per-segment deadline","reply cap derived from declared picture dimensions","decoder pixel limit derived from declared dimensions","a reply picture wider or taller than the container-declared size (the sequence maximum, smaller is legal) but under the reply cap is a worker protocol fault counted in protocol_faults before reel's route sees it (tests oversize_picture_under_the_reply_cap_is_a_protocol_fault, exact_and_smaller_pictures_than_declared_are_accepted, declared_size_is_a_maximum_checked_per_side)","a FRAME answering OPEN is a worker protocol fault counted in protocol_faults, the worker is killed (test frame_reply_to_open_is_a_protocol_fault)","declared dimensions whose byte size overflows are refused as LimitExceeded av1-declared-dimensions before any spawn (test hostile_declared_dims_u32_max_are_refused_before_any_spawn)","reply planes whose declared size overflows are a protocol fault (test frame_dims_u32_max_is_malformed)","cancel kills the worker","the worker handshake wait (HELLO) is polled every 5 ms: a cancel while a worker is starting kills that worker and returns Canceled, and spawn_timeout still bounds the wait (test canceled_call_does_not_wait_for_a_stalled_hello)","the worker executable is looked up in the directory of the running executable and nowhere else (no parent directory, no search path); Av1WorkerOptions::program overrides it (tests worker_is_found_only_beside_the_running_executable, default_discovery_does_not_look_in_the_parent_of_the_executable_directory)"],"reel_mapping":{"decoder_crashed":"ReelError::DecoderFault{Crashed}","decoder_deadline":"ReelError::DecoderFault{Deadline}","worker_protocol":"ReelError::DecoderFault{Protocol}"},"faults":["decoder_crashed","decoder_deadline","decoder_output_too_large","worker_protocol","worker_version_mismatch","worker_spawn_failed","worker_respawn_limited","worker_pool_busy"]},"decoder":{"id":"rav1d","license":"BSD-2-Clause","operator_decision_ref":"START_LOG 2026-10-11T03:30:19Z AV1 lead decision (isolated worker required for untrusted input)"},"not_proven":["operating-system memory or CPU cap for the worker (Windows Job Object needs unsafe FFI)","crash reporter behaviour on aborts that do not reach the Rust panic hook","worker executable packaging next to the host (START_LOG D14)","worker supervision is a copy of motion-js's (R5 cap_duplicate, Operator decision D18 pending)"]}"#;
