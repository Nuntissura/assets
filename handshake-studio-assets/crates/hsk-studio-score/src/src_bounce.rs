//! Bounce over a sample-rate-converting stage (STU-ARC-021): `pre -> ClipResampler -> post`.
use crate::block::{InputBlock, OutputBlock};
use crate::bounce::{
    BounceChunk, BounceReceipt, BounceRequest, BounceRun, ReceiptHeader, identity_digest,
};
use crate::digest::Sha256;
use crate::engine::{BlockReceipt, Engine, Processor, RequestContext};
use crate::error::ScoreError;
use crate::layout::{
    BlockSpec, ChannelLayout, MAX_BLOCK_FRAMES, MAX_CHANNELS, MAX_SAMPLE_RATE_HZ, sample_to_ticks,
    ticks_per_sample,
};
use crate::resample::{ClipResampler, ResampleReceipt, map_start, requested_frames};
use crate::stage::{StageRunner, StageSetup};
use crate::stream_resample::StreamReceipt;
use hsk_studio_accord::{CancellationToken, Ticks};

/// Domain separator of the graph identity (bump the suffix when the scheme changes).
const GRAPH_DOMAIN: &[u8] = b"hsk-studio-score/src-bounce-identity@1";

/// The three stages of one bounce over a sample-rate converter. Pass `Unity` for a stage that
/// should do nothing. All fields are caller-owned and preallocated.
pub struct ResampledBounce<'a> {
    /// Block processor at the INPUT rate; its output layout is `pre_out`'s layout.
    pub pre: &'a mut dyn Processor,
    /// Block scratch for `pre` (capacity at least `block_frames`).
    pub pre_out: &'a mut OutputBlock,
    /// The exact-alignment sample-rate converter; its channel count must equal `pre_out`'s.
    pub resampler: &'a mut ClipResampler,
    /// Block processor at the OUTPUT rate; accepts `pre_out`'s layout and writes `out`'s.
    pub post: &'a mut dyn Processor,
    /// Block scratch for `post` (capacity at least `block_frames`); its layout is the layout of
    /// the chunks the sink receives.
    pub out: &'a mut OutputBlock,
}

/// What kind of stage a [`StageFacts`] row describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageKind {
    Processor,
    Resampler,
}

/// Rate, count and latency facts of one stage of a [`ResampledBounceReceipt`].
///
/// * `Processor`: `input_rate_hz == output_rate_hz`; `input_frames == output_frames` (latency is
///   compensated by rendering `latency_samples` extra silent frames and dropping the first
///   `latency_samples` outputs, so the clip keeps its length); `latency_samples` is the delay that
///   was compensated, in samples of the stage's own rate.
/// * `Resampler`: rates differ or not, `output_frames = ceil(input_frames * out / in)`;
///   `latency_samples` is the NET delay after the alignment pad/drop (0; the residual is about
///   0.01 output frame, see [`ClipResampler`]); the pad and drop counts are in
///   [`ResampledBounceReceipt::resample`].
/// * `identity` = SHA-256 of the stage's canonical configuration string under the chain-identity
///   domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageFacts {
    pub kind: StageKind,
    pub input_rate_hz: u32,
    pub output_rate_hz: u32,
    pub input_frames: u64,
    pub output_frames: u64,
    pub latency_samples: u32,
    pub identity: [u8; 32],
}

impl StageFacts {
    fn new(
        kind: StageKind,
        rates: (u32, u32),
        frames: (u64, u64),
        latency_samples: u32,
        identity: &str,
    ) -> Self {
        Self {
            kind,
            input_rate_hz: rates.0,
            output_rate_hz: rates.1,
            input_frames: frames.0,
            output_frames: frames.1,
            latency_samples,
            identity: identity_digest(identity),
        }
    }
}

/// Exact accounting of one bounce over a sample-rate converter (STU-ARC-021).
///
/// * `requested_input_frames` = input frames; `requested_output_frames = ceil(requested_input_frames
///   * output_rate / input_rate)` in integer arithmetic (u128, never an f64 ratio);
///   `actual_output_frames` = frames delivered to the sink. The bounce is refused unless the three
///   agree; nothing is clamped, truncated or zero-padded. Frames the converter produced past the
///   requested count are reported in `resample.trimmed_tail_frames`.
/// * Input positions are on the input timeline, output positions on the output timeline; the
///   output start is `input_start * out / in` and must be a whole output sample (otherwise the
///   bounce is refused with `OutOfRange`). Ticks are exact for both. The output clip is at most
///   one output sample longer than the input clip (`ceil_excess_ticks`).
/// * `stages` = `[pre, resampler, post]`. `pre` and `post` are the stage receipts (digests of
///   the intermediate and final streams); the final stream is `post`, so `digest`, `labels`,
///   `peak` and `rms` describe the samples the sink received.
/// * `graph_identity` = `SHA-256("hsk-studio-score/src-bounce-identity@1" || crate version || 0x00 ||
///   "src_graph:[" || pre identity || " > " || ClipResampler::identity() || " > " || post identity
///   || "]")`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResampledBounceReceipt {
    pub requested_input_frames: u64,
    pub requested_output_frames: u64,
    pub actual_output_frames: u64,
    pub input_rate_hz: u32,
    pub output_rate_hz: u32,
    pub input_start_sample: u64,
    pub input_end_sample: u64,
    pub output_start_sample: u64,
    pub output_end_sample: u64,
    pub input_start_ticks: Ticks,
    pub input_end_ticks: Ticks,
    pub output_start_ticks: Ticks,
    pub output_end_ticks: Ticks,
    pub revision: u64,
    pub stages: [StageFacts; 3],
    pub pre: BounceReceipt,
    pub resample: ResampleReceipt,
    pub post: BounceReceipt,
    pub graph_identity: [u8; 32],
    /// Bytes of working buffers this bounce allocated itself. Streaming: one block of staging per
    /// processor stage plus the converter's chunk buffers, independent of the clip length.
    /// Whole-clip: the buffered input clip plus the converted clip. Excludes the caller-owned
    /// blocks and rubato's internal history.
    pub working_set_bytes: u64,
}

impl ResampledBounceReceipt {
    /// Output channel labels, in layout order.
    pub fn labels(&self) -> &[u16] {
        self.post.labels()
    }
    /// Whole-clip absolute peak per output channel.
    pub fn peak(&self) -> &[f32] {
        self.post.peak()
    }
    /// Whole-clip RMS per output channel.
    pub fn rms(&self) -> &[f32] {
        self.post.rms()
    }
    /// Digest of the delivered (final) stream, scheme in [`BounceReceipt`].
    pub fn digest(&self) -> [u8; 32] {
        self.post.digest
    }
    pub fn digest_hex(&self) -> String {
        self.post.digest_hex()
    }
    pub fn graph_identity_hex(&self) -> String {
        crate::digest::to_hex(&self.graph_identity)
    }
    pub fn input_duration_ticks(&self) -> u64 {
        self.input_end_ticks.value() - self.input_start_ticks.value()
    }
    pub fn output_duration_ticks(&self) -> u64 {
        self.output_end_ticks.value() - self.output_start_ticks.value()
    }
    /// Ticks by which the whole-sample output clip is longer than the input clip (the `ceil`
    /// of the exact count); always less than one output sample.
    pub fn ceil_excess_ticks(&self) -> u64 {
        self.output_duration_ticks()
            .saturating_sub(self.input_duration_ticks())
    }
}

fn graph_identity(pre: &str, resampler: &str, post: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(GRAPH_DOMAIN);
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(&[0]);
    hasher.update(format!("src_graph:[{pre} > {resampler} > {post}]").as_bytes());
    hasher.finalize()
}

/// The five borrowed parts of a [`ResampledBounce`], in field order.
type StreamStages<'a> = (
    &'a mut dyn Processor,
    &'a mut OutputBlock,
    &'a mut ClipResampler,
    &'a mut dyn Processor,
    &'a mut OutputBlock,
);

/// Everything decided before the first block runs.
struct Plan {
    frames: usize,
    frames_u64: u64,
    requested: usize,
    requested_u64: u64,
    input_rate: u32,
    output_rate: u32,
    mid: ChannelLayout,
    input_start: u64,
    input_end: u64,
    output_start: u64,
    output_end: u64,
    /// `[input start, input end, output start, output end]`.
    ticks: [Ticks; 4],
}

/// Static validation shared by both paths. `bounded` adds the whole-clip limits (input and
/// converted clip at most `MAX_BLOCK_FRAMES` frames).
fn plan(
    request: &BounceRequest<'_>,
    pre: &dyn Processor,
    pre_out: &OutputBlock,
    resampler: &ClipResampler,
    post: &dyn Processor,
    out: &OutputBlock,
    bounded: bool,
) -> Result<Plan, ScoreError> {
    let frames = request.channels.first().map_or(0, |c| c.len());
    if frames == 0 {
        return Err(ScoreError::OutOfRange);
    }
    if request.channels.len() != request.layout.channels() {
        return Err(ScoreError::ChannelMismatch);
    }
    if request.channels.iter().any(|c| c.len() != frames) {
        return Err(ScoreError::LengthMismatch);
    }
    let (input_rate, output_rate) = (request.sample_rate_hz, resampler.output_rate_hz());
    if input_rate == 0 || input_rate > MAX_SAMPLE_RATE_HZ {
        return Err(ScoreError::InvalidRate);
    }
    ticks_per_sample(input_rate)?;
    if resampler.input_rate_hz() != input_rate
        || pre
            .required_sample_rate_hz()
            .is_some_and(|rate| rate != input_rate)
        || post
            .required_sample_rate_hz()
            .is_some_and(|rate| rate != output_rate)
    {
        return Err(ScoreError::InvalidRate);
    }
    let mid = pre_out.layout().clone();
    if !pre.accepts(request.layout, &mid)
        || resampler.channels() != mid.channels()
        || !post.accepts(&mid, out.layout())
    {
        return Err(ScoreError::ChannelMismatch);
    }
    let block = request.block_frames;
    if block == 0 {
        return Err(ScoreError::OutOfRange);
    }
    if block > pre_out.capacity()
        || block > out.capacity()
        || block > MAX_BLOCK_FRAMES
        || pre.max_frames().is_some_and(|max| block > max)
        || post.max_frames().is_some_and(|max| block > max)
    {
        return Err(ScoreError::CapacityExceeded);
    }
    if bounded && frames > MAX_BLOCK_FRAMES {
        return Err(ScoreError::CapacityExceeded);
    }
    if request
        .channels
        .iter()
        .any(|c| c.iter().any(|s| !s.is_finite()))
    {
        return Err(ScoreError::NotFinite);
    }

    let requested = requested_frames(frames, input_rate, output_rate)
        .filter(|count| !bounded || *count <= MAX_BLOCK_FRAMES)
        .ok_or(ScoreError::CapacityExceeded)?;
    let frames_u64 = u64::try_from(frames).map_err(|_| ScoreError::PositionOverflow)?;
    let requested_u64 = u64::try_from(requested).map_err(|_| ScoreError::PositionOverflow)?;
    let input_start = request.start_sample;
    let input_end = input_start
        .checked_add(frames_u64)
        .ok_or(ScoreError::PositionOverflow)?;
    input_start
        .checked_add(frames_u64 + u64::from(pre.latency_samples()))
        .ok_or(ScoreError::PositionOverflow)?;
    let output_start = map_start(input_start, input_rate, output_rate)?;
    let output_end = output_start
        .checked_add(requested_u64)
        .ok_or(ScoreError::PositionOverflow)?;
    output_start
        .checked_add(requested_u64 + u64::from(post.latency_samples()))
        .ok_or(ScoreError::PositionOverflow)?;
    let ticks = [
        sample_to_ticks(input_start, input_rate)?,
        sample_to_ticks(input_end, input_rate)?,
        sample_to_ticks(output_start, output_rate)?,
        sample_to_ticks(output_end, output_rate)?,
    ];
    Ok(Plan {
        frames,
        frames_u64,
        requested,
        requested_u64,
        input_rate,
        output_rate,
        mid,
        input_start,
        input_end,
        output_start,
        output_end,
        ticks,
    })
}

/// The three stage receipts of one run.
struct Parts {
    resample: ResampleReceipt,
    pre: BounceReceipt,
    post: BounceReceipt,
}

/// Cross-checks the counts and assembles the receipt; a disagreement is `LengthMismatch`.
fn assemble(
    ctx: &RequestContext,
    plan: &Plan,
    stages: [&dyn Processor; 2],
    resampler: &ClipResampler,
    parts: Parts,
    working_set_bytes: u64,
) -> Result<ResampledBounceReceipt, ScoreError> {
    let Parts {
        resample,
        pre: pre_receipt,
        post: post_receipt,
    } = parts;
    if resample.requested_output_frames != plan.requested
        || resample.actual_output_frames() != plan.requested
        || pre_receipt.actual_frames != plan.frames_u64
        || post_receipt.requested_frames != plan.requested_u64
        || post_receipt.actual_frames != plan.requested_u64
    {
        return Err(ScoreError::LengthMismatch);
    }
    let [pre, post] = stages;
    let (pre_id, resampler_id, post_id) = (pre.identity(), resampler.identity(), post.identity());
    Ok(ResampledBounceReceipt {
        requested_input_frames: plan.frames_u64,
        requested_output_frames: plan.requested_u64,
        actual_output_frames: post_receipt.actual_frames,
        input_rate_hz: plan.input_rate,
        output_rate_hz: plan.output_rate,
        input_start_sample: plan.input_start,
        input_end_sample: plan.input_end,
        output_start_sample: plan.output_start,
        output_end_sample: plan.output_end,
        input_start_ticks: plan.ticks[0],
        input_end_ticks: plan.ticks[1],
        output_start_ticks: plan.ticks[2],
        output_end_ticks: plan.ticks[3],
        revision: ctx.expected_revision,
        stages: [
            StageFacts::new(
                StageKind::Processor,
                (plan.input_rate, plan.input_rate),
                (plan.frames_u64, pre_receipt.actual_frames),
                pre.latency_samples(),
                &pre_id,
            ),
            StageFacts::new(
                StageKind::Resampler,
                (plan.input_rate, plan.output_rate),
                (plan.frames_u64, plan.requested_u64),
                0,
                &resampler_id,
            ),
            StageFacts::new(
                StageKind::Processor,
                (plan.output_rate, plan.output_rate),
                (plan.requested_u64, post_receipt.actual_frames),
                post.latency_samples(),
                &post_id,
            ),
        ],
        pre: pre_receipt,
        resample,
        post: post_receipt,
        graph_identity: graph_identity(&pre_id, &resampler_id, &post_id),
        working_set_bytes,
    })
}

impl Engine {
    /// Renders a clip through `pre` (input rate), the exact-alignment `resampler`, and `post`
    /// (output rate) in blocks and returns the exact receipt. The clip length is not bounded by
    /// `MAX_BLOCK_FRAMES`: the converter streams ([`ClipResampler::begin_stream`]) and every
    /// stage works on one block at a time, so the working set is a few blocks plus the
    /// converter's chunk buffers whatever the clip length (`receipt.working_set_bytes`).
    ///
    /// Each processor stage is latency compensated as in [`Engine::bounce`] and has the same block
    /// boundaries it would have there; the converted samples are bit-identical to
    /// [`Engine::bounce_resampled_whole_clip`] (the whole-clip oracle) for any clip that fits
    /// both. The sink receives contiguous chunks of the final stream on the OUTPUT timeline, the
    /// first starting at `request.start_sample * out / in` (must be a whole output sample).
    ///
    /// Every statically decidable refusal (counts, rates, layouts, capacities, positions, finite
    /// input, a converter that cannot produce the exact count) happens before the first block
    /// runs; the sink is called only by the final stage. Cancellation and revision are re-checked
    /// before every block of both processor stages, the engine slot is held throughout, and
    /// every processor and the converter are reset on any failure. The stages run interleaved, so
    /// chunks already delivered are provisional: only a returned receipt makes the bounce final
    /// (a non-finite sample produced by `pre` surfaces as `NotFinite` after earlier chunks).
    pub fn bounce_resampled(
        &self,
        ctx: &RequestContext,
        request: &BounceRequest<'_>,
        graph: ResampledBounce<'_>,
        sink: &mut dyn FnMut(BounceChunk<'_>) -> Result<(), ScoreError>,
        cancel: &CancellationToken,
    ) -> Result<ResampledBounceReceipt, ScoreError> {
        let _slot = self.admit(ctx, cancel)?;
        let ResampledBounce {
            pre,
            pre_out,
            resampler,
            post,
            out,
        } = graph;
        let plan = plan(request, &*pre, &*pre_out, &*resampler, &*post, &*out, false)?;
        let out_labels = out.layout().labels().to_vec();
        resampler.begin_stream(plan.frames)?;

        let streamed = self.stream_graph(
            ctx,
            request,
            &plan,
            (
                &mut *pre,
                &mut *pre_out,
                &mut *resampler,
                &mut *post,
                &mut *out,
            ),
            sink,
            cancel,
        );
        let (pre_run, stream, post_run, staging_bytes) = match streamed {
            Ok(done) => done,
            Err(error) => {
                pre.reset();
                post.reset();
                resampler.abort_stream();
                return Err(error);
            }
        };
        let mid_labels = plan.mid.labels();
        let pre_receipt = pre_run.into_receipt(&ReceiptHeader {
            requested_frames: plan.frames_u64,
            sample_rate_hz: plan.input_rate,
            start_sample: plan.input_start,
            end_sample: plan.input_end,
            start_ticks: plan.ticks[0],
            end_ticks: plan.ticks[1],
            revision: ctx.expected_revision,
            latency_dropped_frames: pre.latency_samples(),
            chain_identity: identity_digest(&pre.identity()),
            labels: mid_labels,
        });
        let post_receipt = post_run.into_receipt(&ReceiptHeader {
            requested_frames: plan.requested_u64,
            sample_rate_hz: plan.output_rate,
            start_sample: plan.output_start,
            end_sample: plan.output_end,
            start_ticks: plan.ticks[2],
            end_ticks: plan.ticks[3],
            revision: ctx.expected_revision,
            latency_dropped_frames: post.latency_samples(),
            chain_identity: identity_digest(&post.identity()),
            labels: &out_labels,
        });
        let mut block = BlockReceipt {
            processed_frames: plan.requested,
            latency_samples: 0,
            sample_rate_hz: plan.output_rate,
            start_sample: plan.output_start,
            end_sample: plan.output_end,
            revision: ctx.expected_revision,
            channels: plan.mid.channels(),
            labels: [0; MAX_CHANNELS],
            peak: [0.0; MAX_CHANNELS],
            rms: [0.0; MAX_CHANNELS],
        };
        block.labels[..mid_labels.len()].copy_from_slice(mid_labels);
        block.peak[..stream.peak().len()].copy_from_slice(stream.peak());
        block.rms[..stream.rms().len()].copy_from_slice(stream.rms());
        let resample = ResampleReceipt {
            block,
            input_frames: plan.frames,
            input_rate_hz: plan.input_rate,
            requested_output_frames: plan.requested,
            delay_trimmed_frames: stream.delay_trimmed_frames,
            pad_input_frames: stream.pad_input_frames,
            drop_output_frames: stream.drop_output_frames,
            trimmed_tail_frames: stream.trimmed_tail_frames,
        };
        let working_set = (staging_bytes + stream.buffer_bytes) as u64;
        let parts = Parts {
            resample,
            pre: pre_receipt,
            post: post_receipt,
        };
        let receipt = assemble(ctx, &plan, [&*pre, &*post], resampler, parts, working_set);
        if receipt.is_err() {
            pre.reset();
            post.reset();
        }
        receipt
    }

    /// Runs the three stages interleaved block by block; returns the stage meters, the
    /// converter receipt and the bytes of processor staging held.
    #[allow(clippy::type_complexity)]
    fn stream_graph(
        &self,
        ctx: &RequestContext,
        request: &BounceRequest<'_>,
        plan: &Plan,
        stages: StreamStages<'_>,
        sink: &mut dyn FnMut(BounceChunk<'_>) -> Result<(), ScoreError>,
        cancel: &CancellationToken,
    ) -> Result<(BounceRun, StreamReceipt, BounceRun, usize), ScoreError> {
        let (pre, pre_out, resampler, post, out) = stages;
        let setup_pre = StageSetup {
            engine: self,
            ctx,
            cancel,
            layout: request.layout,
            sample_rate_hz: plan.input_rate,
            start_sample: plan.input_start,
            block_frames: request.block_frames,
            frames: plan.frames,
        };
        let setup_post = StageSetup {
            layout: &plan.mid,
            sample_rate_hz: plan.output_rate,
            start_sample: plan.output_start,
            frames: plan.requested,
            ..setup_pre
        };
        let mut pre_run = StageRunner::new(&setup_pre, pre, pre_out);
        let mut post_run = StageRunner::new(&setup_post, post, out);
        let staging_bytes = pre_run.staging_bytes() + post_run.staging_bytes();
        let output_start = plan.output_start;
        let mut forward = |offset: u64, planes: &[&[f32]]| -> Result<(), ScoreError> {
            sink(BounceChunk {
                start_sample: output_start + offset,
                channels: planes,
            })
        };
        let mut to_post =
            |planes: &[&[f32]]| -> Result<(), ScoreError> { post_run.push(planes, &mut forward) };
        pre_run.push(request.channels, &mut |_, planes| {
            resampler.push_stream(planes, &mut to_post)
        })?;
        let pre_stats =
            pre_run.finish(&mut |_, planes| resampler.push_stream(planes, &mut to_post))?;
        let stream = resampler.finish_stream(&mut to_post)?;
        let post_stats = post_run.finish(&mut forward)?;
        Ok((pre_stats, stream, post_stats, staging_bytes))
    }

    /// The whole-clip oracle of [`Engine::bounce_resampled`]: the same graph and the same
    /// receipt, but the converter works on the buffered clip in one call ([`Engine::resample`]),
    /// so the input clip and the converted clip must each fit `MAX_BLOCK_FRAMES` frames (about
    /// 23.8 s at 44.1 kHz) or the bounce is refused with `CapacityExceeded` before anything
    /// runs. Pass 1 renders `pre` into a buffer, pass 2 converts, pass 3 renders `post` into the
    /// sink; the sink is called only after the converter finished.
    pub fn bounce_resampled_whole_clip(
        &self,
        ctx: &RequestContext,
        request: &BounceRequest<'_>,
        graph: ResampledBounce<'_>,
        sink: &mut dyn FnMut(BounceChunk<'_>) -> Result<(), ScoreError>,
        cancel: &CancellationToken,
    ) -> Result<ResampledBounceReceipt, ScoreError> {
        let _slot = self.admit(ctx, cancel)?;
        let ResampledBounce {
            pre,
            pre_out,
            resampler,
            post,
            out,
        } = graph;
        let plan = plan(request, &*pre, &*pre_out, &*resampler, &*post, &*out, true)?;
        let mut converted = OutputBlock::with_capacity(
            plan.mid.clone(),
            resampler.output_capacity_for(plan.frames),
        )?;

        // Pass 1: the input-rate stage, collected on the input timeline (no sink involved).
        let mut staged: Vec<Vec<f32>> = (0..plan.mid.channels())
            .map(|_| Vec::with_capacity(plan.frames))
            .collect();
        let pre_receipt = {
            let mut collect = |chunk: BounceChunk<'_>| -> Result<(), ScoreError> {
                for (dst, src) in staged.iter_mut().zip(chunk.channels) {
                    dst.extend_from_slice(src);
                }
                Ok(())
            };
            self.bounce_held(ctx, request, &mut *pre, &mut *pre_out, &mut collect, cancel)?
        };
        let rest = self.resampled_tail(
            ctx,
            request,
            &mut *resampler,
            &mut *post,
            &mut *out,
            &mut converted,
            &staged,
            plan.output_start,
            sink,
            cancel,
        );
        let (resample, post_receipt) = match rest {
            Ok(done) => done,
            Err(error) => {
                pre.reset();
                return Err(error);
            }
        };
        let working_set =
            ((staged.len() * plan.frames + converted.backing().len()) * size_of::<f32>()) as u64;
        let parts = Parts {
            resample,
            pre: pre_receipt,
            post: post_receipt,
        };
        let receipt = assemble(ctx, &plan, [&*pre, &*post], resampler, parts, working_set);
        if receipt.is_err() {
            pre.reset();
            post.reset();
        }
        receipt
    }

    /// Passes 2 and 3 of [`Engine::bounce_resampled_whole_clip`]: convert the staged clip, then
    /// run the output-rate stage into the sink, starting at `output_start` on the output
    /// timeline.
    #[allow(clippy::too_many_arguments)]
    fn resampled_tail(
        &self,
        ctx: &RequestContext,
        request: &BounceRequest<'_>,
        resampler: &mut ClipResampler,
        post: &mut dyn Processor,
        out: &mut OutputBlock,
        converted: &mut OutputBlock,
        staged: &[Vec<f32>],
        output_start: u64,
        sink: &mut dyn FnMut(BounceChunk<'_>) -> Result<(), ScoreError>,
        cancel: &CancellationToken,
    ) -> Result<(ResampleReceipt, BounceReceipt), ScoreError> {
        let frames = staged.first().map_or(0, Vec::len);
        let spec = BlockSpec {
            sample_rate_hz: request.sample_rate_hz,
            layout: converted.layout().clone(),
            frames,
            start_sample: request.start_sample,
        };
        let views: Vec<&[f32]> = staged.iter().map(Vec::as_slice).collect();
        let resample = self.resample_held(
            ctx,
            &InputBlock::new(&spec, &views),
            resampler,
            converted,
            cancel,
        )?;
        let published: Vec<&[f32]> = (0..converted.layout().channels())
            .map(|ch| converted.published_channel(ch))
            .collect::<Option<_>>()
            .ok_or(ScoreError::ChannelMismatch)?;
        let layout = converted.layout().clone();
        let tail_request = BounceRequest {
            sample_rate_hz: resampler.output_rate_hz(),
            layout: &layout,
            start_sample: output_start,
            channels: &published,
            block_frames: request.block_frames,
        };
        let post_receipt = self.bounce_held(ctx, &tail_request, post, out, sink, cancel)?;
        Ok((resample, post_receipt))
    }
}
