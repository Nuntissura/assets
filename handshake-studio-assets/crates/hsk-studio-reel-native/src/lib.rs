//! Optional native media codec provider; isolate FFI/toolchain/codec license/version/process
//! boundaries from pure media contracts.
//!
//! Default build: empty and pure (`forbid(unsafe_code)`, no dependencies). Feature `av1` adds the
//! process boundary for AV1 decode:
//! * `reel-native-av1-worker` (binary) hosts reel's rav1d decoder (`hsk_studio_reel::av1::Av1Decoder`)
//!   in its own process;
//! * `WorkerAv1Provider` is a reel `MediaProvider` (id `av1`) that indexes and demuxes the container
//!   through reel's own providers, sends the codec configuration and temporal units to a worker over a
//!   versioned length-prefixed stdin/stdout protocol, and delivers the returned pictures and receipts
//!   with the same vocabulary as the in-process decoder.
//!
//! Reason (reel lane SMB-REEL-ENG-3): rav1d 1.1.0 aborts the whole process on a few percent of corrupted
//! bitstreams (an uncatchable `unwrap` inside an `extern "C"` function), so untrusted AV1 must never be
//! decoded in the caller's process. A worker crash, hang, protocol violation or oversize reply becomes a
//! typed `Av1WorkerFault` (`decoder_crashed`, `decoder_deadline`, ...) and the provider stays usable with
//! bounded respawn.
//!
//! NOTE for integrators: Cargo features are per package, so `av1` also links reel's in-process `av1`
//! feature into the host image (it is never called by this crate). Reel's providers still decode `av01`
//! in-process when asked for `OutputMode::Decoded` through the `isobmff`/`matroska` ids; route untrusted
//! AV1 only through the `av1` id of this provider (see lane notes for the requested reel hook).
#![forbid(unsafe_code)]

#[cfg(feature = "av1")]
mod host;
#[cfg(feature = "av1")]
mod wire;
#[cfg(feature = "av1")]
mod worker;

#[cfg(feature = "av1")]
pub use host::{
    AV1_PROVIDER_ID, Av1ConfigError, Av1Error, Av1WorkerFault, Av1WorkerOptions, Av1WorkerStats,
    WORKER_PROGRAM, WorkerAv1Provider,
};
#[cfg(feature = "av1")]
pub use worker::{PANIC_EXIT_CODE, WorkerTestFault, worker_main};

pub const DESCRIPTOR: &str = r#"{"crate":"hsk-studio-reel-native","module":"reel_native","default_closure":"empty, pure Rust, forbid(unsafe_code), no dependencies","features":{"av1":"optional, default off: AV1 decode in an isolated worker process (reel-native-av1-worker hosting rav1d via hsk-studio-reel) behind WorkerAv1Provider, provider id av1"},"isolation":{"model":"one request per packet over versioned length-prefixed stdin/stdout, host owns and reaps the child, no in-process fallback","spawn":"CREATE_NO_WINDOW on Windows, empty environment, null stderr","bounds":["max_workers","bounded respawn per window","per-request batch deadline","per-segment deadline","reply cap derived from declared picture dimensions","decoder pixel limit derived from declared dimensions","cancel kills the worker"],"faults":["decoder_crashed","decoder_deadline","decoder_output_too_large","worker_protocol","worker_version_mismatch","worker_spawn_failed","worker_respawn_limited","worker_pool_busy"]},"decoder":{"id":"rav1d","license":"BSD-2-Clause","operator_decision_ref":"START_LOG 2026-10-11T03:30:19Z AV1 lead decision (isolated worker required for untrusted input)"},"not_proven":["operating-system memory or CPU cap for the worker (Windows Job Object needs unsafe FFI)","crash reporter behaviour on aborts that do not reach the Rust panic hook","worker executable packaging next to the host (START_LOG D14)"]}"#;
