//! Pure audio sample/channel/timing, DSP/spectral/mix/routing/latency semantics and bounded offline
//! block processing. Device I/O and plugin hosting are optional separate providers.
//!
//! Model: planar `f32` blocks. A caller preallocates an [`OutputBlock`]; [`Engine::process`]
//! admits one [`InputBlock`] (refusing before any write), runs a [`Processor`], and publishes the
//! whole block with a [`BlockReceipt`] or discards it unpublished. Positions are integer sample
//! indices; [`sample_to_ticks`] converts exactly to `hsk_studio_accord::Ticks`.
#![forbid(unsafe_code)]

mod block;
mod bounce;
mod chain;
mod digest;
mod dsp;
mod dynamics;
mod engine;
mod eq;
mod error;
mod layout;
mod loudness;
mod mix;
mod ramp;
mod report;
mod resample;
mod reverb;
mod spectrum;
mod src_bounce;
mod stage;
mod stream_resample;
mod truepeak;

pub use block::{InputBlock, OutputBlock};
pub use bounce::{BounceChunk, BounceReceipt, BounceRequest};
pub use chain::{Chain, ChainStage, MAX_CHAIN_STAGES};
pub use digest::{sha256, to_hex};
pub use dsp::{
    ChannelMix, Gain, HARD_LIMITER_INPUT_BOOST_DB, HARD_LIMITER_LOOKAHEAD_MS,
    HARD_LIMITER_MAX_AMP_DB, HARD_LIMITER_RELEASE_MS, HardLimiter, HardLimiterParams, Pan, Unity,
    db_to_linear, linear_to_db, normalised_to_value, value_to_normalised,
};
pub use dynamics::{
    DYNAMICS_ATTACK_MS, DYNAMICS_HOLD_MS, DYNAMICS_KNEE_DB, DYNAMICS_MAKEUP_DB, DYNAMICS_RANGE_DB,
    DYNAMICS_RATIO, DYNAMICS_RELEASE_MS, DYNAMICS_RMS_WINDOW_MS, DYNAMICS_THRESHOLD_DB, Detector,
    Dynamics, DynamicsMode, DynamicsParams, LIMITER_CEILING_DB, LIMITER_LOOKAHEAD_MS,
    LIMITER_RELEASE_MS, LimiterParams, LookaheadLimiter,
};
pub use engine::{BlockReceipt, Engine, Processor, RequestContext, Reservation};
pub use eq::{
    BiquadKind, BiquadSpec, EQ_AMP_DB, EQ_FREQUENCY_MIN_HZ, EQ_MASTER_GAIN_DB, EQ_Q, Equalizer,
    MAX_EQ_SECTIONS,
};
pub use error::ScoreError;
pub use layout::{
    BlockSpec, CANCEL_CHECK_FRAMES, ChannelLayout, DEFAULT_SAMPLE_RATE_HZ, MAX_BLOCK_FRAMES,
    MAX_CHANNELS, MAX_SAMPLE_RATE_HZ, STEREO_LABELS, sample_to_ticks, ticks_per_sample,
    ticks_to_sample,
};
pub use loudness::{
    LoudnessMeter, LoudnessReadout, LoudnessSnapshot, SURROUND_WEIGHT, k_weighting_coefficients,
};
pub use mix::{MAX_SENDS, SendRoute};
pub use ramp::RampedGain;
pub use report::{outcome_of, report};
pub use resample::{ClipResampler, MIN_CHUNK_FRAMES, ResampleReceipt, SINC_LEN};
pub use reverb::{
    REVERB_DAMPING, REVERB_LEVEL, REVERB_PRE_DELAY_MS, REVERB_RT60_S, REVERB_WIDTH, Reverb,
    ReverbParams,
};
pub use spectrum::{MAX_FFT_SIZE, MIN_FFT_SIZE, SpectrumAnalyzer};
pub use src_bounce::{ResampledBounce, ResampledBounceReceipt, StageFacts, StageKind};
pub use stream_resample::{StreamEmit, StreamReceipt};

/// Manual and inspection descriptor for a model or tool with no prior context.
pub const DESCRIPTOR: &str = r#"{"crate":"hsk-studio-score","schema":"hsk.studio.score.descriptor@1","purpose":"Pure audio sample/channel/timing, DSP/mix/routing/latency semantics and bounded offline block processing; no device I/O, plugin hosting, decoding or database.","sample_format":"planar f32, one slice per channel","limits":{"max_channels":16,"max_block_frames":1048576,"max_sample_rate_hz":768000,"cancel_check_frames":256},"timing":{"position":"integer sample index","ticks_per_second":254016000000,"rate_rule":"rate must divide ticks_per_second exactly and be <= max_sample_rate_hz; 48000 -> 5292000 ticks/sample; every entry point (block admission, bounces, converter, HardLimiter, Equalizer) refuses other rates, e.g. 44101 or 47999, with invalid_rate","output_guard":"a processed block, mix, converted clip, converter chunk or spectrum that holds NaN or infinity (finite input can overflow f32, e.g. gain f32::MAX on 2.0) is discarded unpublished with not_finite before any sink or caller reads it; a Chain checks every stage output","latency":"receipt.latency_samples = processor delay; receipt.peak()/rms() = per-channel meters of the published block; block content is delayed by it relative to the input timeline"},"flow":["Engine::new(resource, revision)","OutputBlock::with_capacity(layout, capacity_frames) once","RequestContext{resource, actor: Some(..), expected_revision}","Engine::process(ctx, InputBlock, &mut Processor, &mut OutputBlock, &CancellationToken) -> BlockReceipt","report(result, correlation_id, revision, cancel, Observe, SinkPort)"],"admission_order":["cancel","missing_context","resource_mismatch","stale_revision","busy","frames/rate/channels/lengths/position overflow/non-finite","processor rate and layout","capacity"],"processors":{"Unity":"bit-exact copy, latency 0","Gain":"out = in * linear","RampedGain":"linear per-block gain ramp current->target (frame n uses current+(target-current)*n/N); block discard reverts","SpectrumAnalyzer":"Hann + radix-2 FFT amplitude spectrum, power-of-two sizes 8..65536; bin-centred sine of amplitude A reads A (Hann neighbours A/2)","Pan":"mono -> stereo equal power, pan -1..1","ChannelMix":"linear channel matrix (stereo_to_mono, mono_to_stereo, custom)","Engine::bounce":"renders a whole clip through a Processor in blocks (latency compensated) and returns a BounceReceipt: requested vs actual frames, exact start/end ticks, whole-clip peak/rms, revision, chain_identity (SHA-256 of Processor::identity) and a block-size-independent SHA-256 sample digest (scheme in the BounceReceipt docs)","Engine::bounce_resampled":"bounce over a sample-rate-converting graph pre(input rate) -> ClipResampler -> post(output rate), STREAMING: every stage works block by block (ClipResampler::begin_stream/push_stream/finish_stream) so the clip length is not bounded by max_block_frames and the working set (receipt.working_set_bytes) is a few blocks plus the converter chunk buffers; converted samples are bit-identical to the whole-clip oracle Engine::bounce_resampled_whole_clip (clips <= max_block_frames); sink gets output-timeline chunks starting at start_sample*out/in (must be exact); ResampledBounceReceipt: requested_input_frames, requested_output_frames = ceil(in*out_rate/in_rate) in u128, actual_output_frames (all three must agree or the bounce is refused), exact input and output ticks, ceil_excess_ticks, per-stage rate/frames/latency facts [pre, resampler, post], resample pad/drop/trimmed_tail, graph_identity, digest of the delivered stream","ClipResampler streaming":"begin_stream(input_frames) sizes the chunk buffers once, push_stream(planar pieces, emit) / finish_stream(emit) -> StreamReceipt; same rubato call sequence as Engine::resample so output is bit-identical; any error ends the stream (abort_stream)","Chain":"ordered effect stack that is itself a Processor; per-stage bypass (layout-preserving stages only); latency = sum of active stages; scratch preallocated","ClipResampler":"fixed-ratio sinc sample-rate converter for whole clips via Engine::resample (rubato Async sinc; input zero-padded and output frames dropped so output frame m is the input value at m/ratio, residual about 0.01 frame); output count is exactly ceil(frames*out/in) in integer math; receipt reports requested vs actual output frames, pad and drop counts and the trimmed tail","Equalizer":"cascade of RBJ cookbook biquads (peaking, low/high shelf, low/high pass) in f64 plus master gain; STU-FX-136a ranges frequency 20..rate/2, amp -48..48, Q 0.0001..10000, master -96..48; magnitude_db() gives the analytic response","Engine::mix":"ordered SendRoute list summed into a submix: out = sum(level_i * send_i); sends must share layout, rate, start sample and frames","HardLimiter":"lookahead brickwall limiter; ranges max_amp -100..0 dB, input_boost -100..50 dB, lookahead 5..20 ms, release 40..200 ms; units authored (spec declares none)"},"errors":["invalid_rate","invalid_layout","channel_mismatch","length_mismatch","capacity_exceeded","position_overflow","busy","canceled","stale_revision","missing_context","not_finite","out_of_range","resource_mismatch","resample_failed","timeline_mismatch"],"recovery":{"busy":"release the Reservation or wait for the in-flight block; never spin","canceled":"nothing was published; the processor was reset; resubmit","stale_revision":"re-read Engine::revision and resubmit","capacity_exceeded":"allocate a larger OutputBlock before the callback"},"not_in_scope":["device I/O (score-device)","plugin hosting (score-plugin)","file decode/encode (reel)","loudness standards (spec gap STU-FX-147)","Folio document mapping"]}"#;
