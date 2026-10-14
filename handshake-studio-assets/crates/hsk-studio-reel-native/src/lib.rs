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
// Default and host builds: no unsafe. With the opt-in `mf-worker` feature only `mf/ffi.rs` may use it
// (Media Foundation COM calls); every other module still refuses it.
#![cfg_attr(not(feature = "mf-worker"), forbid(unsafe_code))]
#![cfg_attr(feature = "mf-worker", deny(unsafe_code))]

#[cfg(any(feature = "av1", feature = "mf"))]
mod host;
#[cfg(feature = "mf")]
mod mf;
#[cfg(feature = "mf")]
mod mf_host;
#[cfg(all(any(feature = "av1", feature = "mf"), feature = "test-faults"))]
mod test_fault;
#[cfg(any(feature = "av1", feature = "mf"))]
mod wire;
#[cfg(feature = "av1-worker")]
mod worker;

#[cfg(any(feature = "av1", feature = "mf"))]
pub use host::{
    AV1_PROVIDER_ID, Av1ConfigError, Av1Error, Av1WorkerFault, Av1WorkerOptions, Av1WorkerStats,
    WORKER_PROGRAM, WorkerAv1Provider,
};
#[cfg(all(any(feature = "av1", feature = "mf"), feature = "test-faults"))]
pub use test_fault::WorkerTestFault;
#[cfg(feature = "mf")]
pub use mf_host::{
    MF_DXVA_UNAVAILABLE, MF_H264_PROVIDER_ID, MF_MAX_HEIGHT, MF_MAX_WIDTH, MF_MIN_SIDE,
    MF_WORKER_PROGRAM, MfConfigError, MfPath, MfWorkerOptions, MfWorkerStats,
    WorkerMfProvider,
};
#[cfg(all(feature = "mf-worker", windows))]
pub use mf::worker::{MF_DECODER_ID, MF_PANIC_EXIT_CODE, mf_worker_main};
#[cfg(feature = "av1-worker")]
pub use worker::{PANIC_EXIT_CODE, worker_main};

pub const DESCRIPTOR: &str = r#"{"crate":"hsk-studio-reel-native","module":"reel_native","default_closure":"empty, pure Rust, forbid(unsafe_code), no dependencies","features":{"av1":"optional, default off, host side: WorkerAv1Provider (reel ExternalVideoDecoder via Registry::with_video_decoder, and provider id av1) driving reel-native-av1-worker processes; uses hsk-studio-reel without its av1 feature, so the host does not link rav1d; decoder id checked and decoder version learned from the worker HELLO (version reported as unknown until the first worker completed HELLO)","av1-worker":"optional, default off: the reel-native-av1-worker binary hosting rav1d via hsk-studio-reel feature av1; build the worker executable with it","mf":"optional, default off, host side, pure Rust: WorkerMfProvider (reel ExternalVideoDecoder for VideoCodec::H264, avc1/avc3 ISO BMFF) driving reel-native-mf-worker processes on the same pool core as AV1; pre-spawn refusals for avcC, profile (66/77/100), chroma (4:2:0), bit depth (8), declared-size overflow, frame budget and the Microsoft decoder's 48..=4096 x 48..=2304 (test mf_declared_limits_and_unsupported_streams); ISO BMFF samples converted to Annex B with SPS/PPS before the first and every IDR packet; the decoder releases pictures in presentation order (B-frames: Main and High with 2 or 3 B-frames, b-pyramid, edit list and negative composition offsets) and its sessions declare reel reorders_output, so every picture is matched to its own packet and delivered in presentation order, exact per frame vs ffmpeg on the software path and on DXVA where a Direct3D 11 video device exists (test h264_bframe_streams_decode_exact_in_presentation_order); paths MfPath Software (default), Hardware (DXVA, Unsupported mf-dxva-unavailable where it cannot, every picture must be a Direct3D surface), Auto (hardware then software); heartbeat watch: the worker's decode loop advances a HEARTBEAT counter, a counter that does not advance within MfWorkerOptions::heartbeat_timeout (default 2 s) kills the worker (decoder_deadline, killed_heartbeat) and a counter that does not increase is a protocol fault; bounded replay: after a crash or deadline the segment resumes once on a fresh worker from the last IDR packet (retention cap replay_bytes, default 64 MiB), pictures already delivered are dropped (test mf_heartbeat_and_replay)","mf-worker":"optional, default off, Windows only: the reel-native-mf-worker binary hosting the OS-provided Windows Media Foundation H.264 decoder MFT (MFStartup + MFTEnumEx, first synchronous software decoder MFT; DXVA through a headless Direct3D 11 video device and a DXGI device manager when the pool asks for it); unsafe confined to src/mf/ffi.rs; software output equals the ffmpeg oracle per frame (test h264_baseline_software_decode_exact_vs_oracle) and DXVA output equals it on this host (test mf_dxva_parity_with_software, NOT_PROVEN without a Direct3D 11 video device)","test-faults":"tests only, never in product builds: worker fault injection (WorkerTestFault, Av1WorkerOptions::test_fault); without it the worker does not read HSK_REEL_NATIVE_AV1_WORKER_TEST_FAULT (test worker_fault_env)"},"isolation":{"model":"one request per packet over versioned length-prefixed stdin/stdout, host owns and reaps the child, no in-process fallback","spawn":"CREATE_NO_WINDOW on Windows, empty environment, null stderr","bounds":["max_workers","bounded respawn per window","per-request batch deadline","per-segment deadline","reply cap derived from declared picture dimensions","decoder pixel limit derived from declared dimensions","a reply picture wider or taller than the container-declared size (the sequence maximum, smaller is legal) but under the reply cap is a worker protocol fault counted in protocol_faults before reel's route sees it (tests oversize_picture_under_the_reply_cap_is_a_protocol_fault, exact_and_smaller_pictures_than_declared_are_accepted, declared_size_is_a_maximum_checked_per_side)","a FRAME answering OPEN is a worker protocol fault counted in protocol_faults, the worker is killed (test frame_reply_to_open_is_a_protocol_fault)","a reply FRAME whose tag no PACKET of the session carried is a worker protocol fault counted in protocol_faults once per call, the worker is killed, no picture reaches the sink (test frame_with_an_unsent_tag_is_a_protocol_fault)","more than 64 reply FRAMEs in one batch (MAX_FRAMES_PER_BATCH) are a worker protocol fault counted in protocol_faults once per call, the worker is killed (test more_frames_than_a_batch_may_hold_is_a_protocol_fault)","a BATCH_END whose count differs from the FRAMEs received is a worker protocol fault counted in protocol_faults once per call, the worker is killed (test batch_end_count_differing_from_the_frames_received_is_a_protocol_fault)","OPENED answering a PACKET or a FINISH is a worker protocol fault counted in protocol_faults once per call, the worker is killed (tests opened_reply_to_a_packet_is_a_protocol_fault, opened_reply_to_finish_is_a_protocol_fault)","a HELLO whose first four bytes read as a length above the handshake cap (1024 bytes), an oversize frame or bytes that are no frame such as a text banner, or a frame within the cap that the host cannot decode, is a worker protocol fault counted in protocol_faults and typed worker_protocol, not worker_spawn_failed (tests oversize_hello_is_a_protocol_fault_not_a_spawn_failure, garbage_hello_is_a_protocol_fault_not_a_spawn_failure, malformed_hello_is_a_protocol_fault_not_a_spawn_failure); a worker that closes its output before the HELLO is complete (nothing, fewer than four bytes, or fewer bytes than the length it announced), stays silent past spawn_timeout or cannot be started is worker_spawn_failed counted in spawn_failed (tests a_worker_that_never_greets_is_a_spawn_failure_not_a_protocol_fault, a_greeting_cut_short_is_a_spawn_failure_not_a_protocol_fault, canceled_call_does_not_wait_for_a_stalled_hello)","declared dimensions whose byte size overflows are refused as LimitExceeded av1-declared-dimensions before any spawn (test hostile_declared_dims_u32_max_are_refused_before_any_spawn)","reply planes whose declared size overflows are a protocol fault (test frame_dims_u32_max_is_malformed)","cancel kills the worker","the worker handshake wait (HELLO) is polled every 5 ms: a cancel while a worker is starting kills that worker and returns Canceled, and spawn_timeout still bounds the wait (test canceled_call_does_not_wait_for_a_stalled_hello)","the worker executable is looked up in the directory of the running executable and nowhere else (no parent directory, no search path); Av1WorkerOptions::program overrides it (tests worker_is_found_only_beside_the_running_executable, default_discovery_does_not_look_in_the_parent_of_the_executable_directory)"],"reel_mapping":{"decoder_crashed":"ReelError::DecoderFault{Crashed}","decoder_deadline":"ReelError::DecoderFault{Deadline}","worker_protocol":"ReelError::DecoderFault{Protocol}"},"faults":["decoder_crashed","decoder_deadline","decoder_output_too_large","worker_protocol","worker_version_mismatch","worker_spawn_failed","worker_respawn_limited","worker_pool_busy"]},"decoder":{"id":"rav1d","license":"BSD-2-Clause","operator_decision_ref":"START_LOG 2026-10-11T03:30:19Z AV1 lead decision (isolated worker required for untrusted input)"},"h264_decoder":{"id":"mf-h264","version":"learned from the worker HELLO: <MFT friendly name>|<CLSID>|dxva=0 or dxva=1|os-licensed","licence":"Windows Media Foundation H.264 decoder, OS-licensed","certification":"not_applicable","decode_only":true,"operator_decision_ref":"START_LOG OPERATOR DECISIONS 2026-10-14 D24 (H.264 decode only through the OS-provided Media Foundation decoder in the isolated reel-native worker; typed MEDIA_CODEC_UNAVAILABLE where absent; no openh264, no encode)","dxva_worker_environment":"SystemRoot only (D3D11 device creation fails with DXGI_ERROR_UNSUPPORTED in an empty environment, measured 2026-10-14)"},"not_proven":["operating-system memory or CPU cap for the worker (Windows Job Object needs unsafe FFI)","crash reporter behaviour on aborts that do not reach the Rust panic hook","worker executable packaging next to the host (START_LOG D14)","worker supervision is a copy of motion-js's (R5 cap_duplicate, Operator decision D18 pending)"]}"#;
