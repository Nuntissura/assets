//! Optional native device I/O provider; all configuration and allocation off realtime callback;
//! preallocated fixed-capacity handoff.
//!
//! Model: a caller holds an explicit [`DeviceGrant`], negotiates a [`StreamConfig`] (rate, Score
//! [`ChannelLayout`](hsk_studio_score::ChannelLayout), block frames, ring blocks) against
//! [`Limits`], and a [`DeviceProvider`] opens an [`OutputStream`] or [`InputStream`]. Samples are
//! interleaved `f32`. The control thread owns one ring end ([`Producer`] for output,
//! [`Consumer`] for input); the device callback owns the other and runs only
//! [`render_callback`] / [`capture_callback`], which copy preallocated ring storage and bump
//! relaxed counters. Underrun writes silence; overrun and device loss are typed and counted.
//!
//! The default build is pure Rust (`SoftwareProvider`, a clocked thread). It proves the
//! lifecycle and counters; it is NOT native-device proof.
#![forbid(unsafe_code)]

mod callback;
mod config;
mod counters;
#[cfg(feature = "cpal-backend")]
mod cpal_backend;
mod error;
mod provider;
mod report;
mod ring;
mod software;

pub use callback::{capture_callback, render_callback};
pub use config::{
    DeviceGrant, DeviceId, DeviceInfo, Direction, Limits, MIN_RING_BLOCKS, RingGeometry,
    SAMPLE_BYTES, StreamConfig,
};
pub use counters::{CounterSnapshot, Counters};
#[cfg(feature = "cpal-backend")]
pub use cpal_backend::{CPAL_DEFAULT_OUTPUT, CpalProvider, NativeFormat};
pub use error::{DeviceError, PushError};
pub use provider::{
    Admission, CloseReport, DeviceProvider, InputStream, OutputStream, Permit, StreamBackend,
};
pub use report::{outcome_of, report};
pub use ring::{Consumer, Producer, ring};
pub use software::{
    ClockedSoftwareDevice, SOFTWARE_INPUT_DEVICE, SOFTWARE_MAX_CHANNELS, SOFTWARE_OUTPUT_DEVICE,
    SoftwareOptions, SoftwareProvider,
};

/// Manual and inspection descriptor for a model or tool with no prior context.
pub const DESCRIPTOR: &str = r#"{"crate":"hsk-studio-score-device","schema":"hsk.studio.score-device.descriptor@1","purpose":"Optional device I/O provider port: grant-checked open, negotiated fixed ring, realtime callback bodies that only copy and count, typed underrun/overrun/loss, bounded admission. Default build is pure Rust with a clocked software device; native backends are optional features.","sample_format":"interleaved f32","limits_default":{"max_ring_bytes":4194304,"max_block_frames":16384,"max_ring_blocks":256,"max_open_streams":4,"min_ring_blocks":2},"flow":["DeviceGrant::new(actor, resource, directions, device)","StreamConfig{sample_rate_hz, layout, block_frames, ring_blocks}.validate(limits)","provider.open_output(device, config, Some(&grant), &cancel) -> OutputStream","stream.producer().try_push_block(&block) (exactly block_frames*channels samples; Full = refusal, caller keeps block; DeviceLost = typed loss)","stream.close() -> CloseReport{permits_released, thread_joined, frames_submitted, frames_consumed, residual_frames, underruns, overruns, ...}","report(result, correlation_id, revision, cancel, Observe, SinkPort)"],"refusal_order":["canceled","unauthorized (no grant or wrong direction/device)","unknown_device","invalid_rate","invalid_block_frames","invalid_ring_blocks","ring_too_large","unsupported_layout","admission_denied"],"callback_rules":{"render_callback":"copy available frames, zero-fill the rest (safe underrun output), count underrun and underrun_frames","capture_callback":"push the frames that fit, count one overrun + one discontinuity + dropped_frames for the rest","forbidden":"allocation, waiting, logging, formatting, decoding, state save (STU-ARC-013); enforced by an allocation-counting test and a source scan"},"accounting":"frames_submitted == frames_consumed + residual_frames (checked by CloseReport::reconciles)","counters":["submitted_frames","consumed_frames","underruns","underrun_frames","overruns","dropped_frames","discontinuities","callbacks","device_lost"],"outcomes":{"Success":"clean close, nothing lost","Failure(Loss)":"device lost, capture frames dropped or frames do not reconcile","Failure(Unavailable)":"no device, admission exhausted or backend not quiescent","Failure(Unsupported)":"layout beyond the device","Failure(Validation)":"caller mistake (grant, rate, block, ring)","Canceled":"cancellation observed before any resource was created"},"software_device":{"provider":"SoftwareProvider","devices":["software-clock-out","software-clock-in"],"max_channels":8,"loopback":"SoftwareOptions.loopback_frames preallocates a recorder of what the fake hardware received","proof_class":"lifecycle and counters only; NOT native-device proof"},"features":{"default":"[] (pure Rust)","cpal-backend":"optional native backend, default OFF; native tests are ignored unless STUDIO_NATIVE_AUDIO=1; no audible output; no device means NOT_PROVEN"},"not_in_scope":["resampling","format conversion beyond f32","MIDI","ASIO/JACK/PipeWire/Pulse","record-to-file","plugin hosting (score-plugin)","device-selection UI","latency calibration","multi-device aggregate"],"recovery":{"full":"keep the block and retry after the callback drained; never spin without sleeping","device_lost":"close the stream and open a new one with a fresh grant check","admission_denied":"close another stream first"}}"#;
