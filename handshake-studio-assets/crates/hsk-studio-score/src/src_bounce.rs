//! Bounce over a sample-rate-converting stage (STU-ARC-021): `pre -> ClipResampler -> post`.
use crate::block::{InputBlock, OutputBlock};
use crate::bounce::{BounceChunk, BounceReceipt, BounceRequest, identity_digest};
use crate::digest::Sha256;
use crate::engine::{Engine, Processor, RequestContext};
use crate::error::ScoreError;
use crate::layout::{
    BlockSpec, MAX_BLOCK_FRAMES, MAX_SAMPLE_RATE_HZ, sample_to_ticks, ticks_per_sample,
};
use crate::resample::{ClipResampler, ResampleReceipt, map_start, requested_frames};
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

impl Engine {
    /// Renders a whole clip through `pre` (input rate), the exact-alignment `resampler`, and
    /// `post` (output rate) and returns the exact receipt.
    ///
    /// Each processor stage is latency compensated as in [`Engine::bounce`]; the sink receives
    /// contiguous chunks of the final stream on the OUTPUT timeline, the first starting at
    /// `request.start_sample * out / in`. The converter works on the whole clip
    /// ([`Engine::resample`] semantics), so the input clip and the converted clip must fit the
    /// crate's block bound (`MAX_BLOCK_FRAMES` frames, about 23.8 s at 44.1 kHz); longer clips are
    /// refused with `CapacityExceeded` before anything runs.
    ///
    /// Every statically decidable refusal (counts, rates, layouts, capacities, positions, finite
    /// input) happens before the first block runs; the sink is called only by the final stage.
    /// Cancellation and revision are re-checked between blocks and after the converter, the engine
    /// slot is held throughout, and every processor is reset on any failure. Chunks already
    /// delivered are provisional: only a returned receipt makes the bounce final.
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
        if frames > MAX_BLOCK_FRAMES {
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
            .filter(|count| *count <= MAX_BLOCK_FRAMES)
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
        let ticks = (
            sample_to_ticks(input_start, input_rate)?,
            sample_to_ticks(input_end, input_rate)?,
            sample_to_ticks(output_start, output_rate)?,
            sample_to_ticks(output_end, output_rate)?,
        );
        let mut converted =
            OutputBlock::with_capacity(mid.clone(), resampler.output_capacity_for(frames))?;

        // Pass 1: the input-rate stage, collected on the input timeline (no sink involved).
        let mut staged: Vec<Vec<f32>> = (0..mid.channels())
            .map(|_| Vec::with_capacity(frames))
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
            output_start,
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
        if resample.requested_output_frames != requested
            || resample.actual_output_frames() != requested
            || pre_receipt.actual_frames != frames_u64
            || post_receipt.requested_frames != requested_u64
            || post_receipt.actual_frames != requested_u64
        {
            pre.reset();
            post.reset();
            return Err(ScoreError::LengthMismatch);
        }

        let (pre_id, resampler_id, post_id) =
            (pre.identity(), resampler.identity(), post.identity());
        Ok(ResampledBounceReceipt {
            requested_input_frames: frames_u64,
            requested_output_frames: requested_u64,
            actual_output_frames: post_receipt.actual_frames,
            input_rate_hz: input_rate,
            output_rate_hz: output_rate,
            input_start_sample: input_start,
            input_end_sample: input_end,
            output_start_sample: output_start,
            output_end_sample: output_end,
            input_start_ticks: ticks.0,
            input_end_ticks: ticks.1,
            output_start_ticks: ticks.2,
            output_end_ticks: ticks.3,
            revision: ctx.expected_revision,
            stages: [
                StageFacts::new(
                    StageKind::Processor,
                    (input_rate, input_rate),
                    (frames_u64, pre_receipt.actual_frames),
                    pre.latency_samples(),
                    &pre_id,
                ),
                StageFacts::new(
                    StageKind::Resampler,
                    (input_rate, output_rate),
                    (frames_u64, requested_u64),
                    0,
                    &resampler_id,
                ),
                StageFacts::new(
                    StageKind::Processor,
                    (output_rate, output_rate),
                    (requested_u64, post_receipt.actual_frames),
                    post.latency_samples(),
                    &post_id,
                ),
            ],
            pre: pre_receipt,
            resample,
            post: post_receipt,
            graph_identity: graph_identity(&pre_id, &resampler_id, &post_id),
        })
    }

    /// Passes 2 and 3 of [`Engine::bounce_resampled`]: convert the staged clip, then run the
    /// output-rate stage into the sink, starting at `output_start` on the output timeline.
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
