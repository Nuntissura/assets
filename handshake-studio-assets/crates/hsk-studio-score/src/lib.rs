//! Pure audio sample/channel/timing, DSP/spectral/mix/routing/latency semantics and bounded offline
//! block processing. Device I/O and plugin hosting are optional separate providers.
//!
//! Model: planar `f32` blocks. A caller preallocates an [`OutputBlock`]; [`Engine::process`]
//! admits one [`InputBlock`] (refusing before any write), runs a [`Processor`], and publishes the
//! whole block with a [`BlockReceipt`] or discards it unpublished. Positions are integer sample
//! indices; [`sample_to_ticks`] converts exactly to `hsk_studio_accord::Ticks`.
#![forbid(unsafe_code)]

mod block;
mod dsp;
mod engine;
mod error;
mod layout;
mod report;

pub use block::{InputBlock, OutputBlock};
pub use dsp::{
    ChannelMix, Gain, HARD_LIMITER_INPUT_BOOST_DB, HARD_LIMITER_LOOKAHEAD_MS,
    HARD_LIMITER_MAX_AMP_DB, HARD_LIMITER_RELEASE_MS, HardLimiter, HardLimiterParams, Pan, Unity,
    db_to_linear, linear_to_db, normalised_to_value, value_to_normalised,
};
pub use engine::{BlockReceipt, Engine, Processor, RequestContext, Reservation};
pub use error::ScoreError;
pub use layout::{
    BlockSpec, CANCEL_CHECK_FRAMES, ChannelLayout, DEFAULT_SAMPLE_RATE_HZ, MAX_BLOCK_FRAMES,
    MAX_CHANNELS, MAX_SAMPLE_RATE_HZ, STEREO_LABELS, sample_to_ticks, ticks_per_sample,
    ticks_to_sample,
};
pub use report::{outcome_of, report};

/// Manual and inspection descriptor for a model or tool with no prior context.
pub const DESCRIPTOR: &str = r#"{"crate":"hsk-studio-score","schema":"hsk.studio.score.descriptor@1","purpose":"Pure audio sample/channel/timing, DSP/mix/routing/latency semantics and bounded offline block processing; no device I/O, plugin hosting, decoding or database.","sample_format":"planar f32, one slice per channel","limits":{"max_channels":16,"max_block_frames":1048576,"max_sample_rate_hz":768000,"cancel_check_frames":256},"timing":{"position":"integer sample index","ticks_per_second":254016000000,"rate_rule":"rate must divide ticks_per_second exactly; 48000 -> 5292000 ticks/sample","latency":"receipt.latency_samples = processor delay; block content is delayed by it relative to the input timeline"},"flow":["Engine::new(resource, revision)","OutputBlock::with_capacity(layout, capacity_frames) once","RequestContext{resource, actor: Some(..), expected_revision}","Engine::process(ctx, InputBlock, &mut Processor, &mut OutputBlock, &CancellationToken) -> BlockReceipt","report(result, correlation_id, revision, cancel, Observe, SinkPort)"],"admission_order":["cancel","missing_context","resource_mismatch","stale_revision","busy","frames/rate/channels/lengths/position overflow/non-finite","processor rate and layout","capacity"],"processors":{"Unity":"bit-exact copy, latency 0","Gain":"out = in * linear","Pan":"mono -> stereo equal power, pan -1..1","ChannelMix":"linear channel matrix (stereo_to_mono, mono_to_stereo, custom)","HardLimiter":"lookahead brickwall limiter; ranges max_amp -100..0 dB, input_boost -100..50 dB, lookahead 5..20 ms, release 40..200 ms; units authored (spec declares none)"},"errors":["invalid_rate","invalid_layout","channel_mismatch","length_mismatch","capacity_exceeded","position_overflow","busy","canceled","stale_revision","missing_context","not_finite","out_of_range","resource_mismatch","resample_failed"],"recovery":{"busy":"release the Reservation or wait for the in-flight block; never spin","canceled":"nothing was published; the processor was reset; resubmit","stale_revision":"re-read Engine::revision and resubmit","capacity_exceeded":"allocate a larger OutputBlock before the callback"},"not_in_scope":["device I/O (score-device)","plugin hosting (score-plugin)","file decode/encode (reel)","loudness standards (spec gap STU-FX-147)","Folio document mapping"]}"#;
