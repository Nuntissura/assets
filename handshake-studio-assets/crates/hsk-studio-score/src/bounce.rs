use crate::block::{InputBlock, OutputBlock};
use crate::digest::Sha256;
use crate::engine::{Engine, Processor, RequestContext};
use crate::error::ScoreError;
use crate::layout::{
    BlockSpec, ChannelLayout, MAX_BLOCK_FRAMES, MAX_CHANNELS, MAX_SAMPLE_RATE_HZ, sample_to_ticks,
    ticks_per_sample,
};
use hsk_studio_accord::{CancellationToken, Ticks};

/// Domain separators of the two digests (bump the suffix when a scheme changes).
const DIGEST_DOMAIN: &[u8] = b"hsk-studio-score/bounce-digest@1";
const IDENTITY_DOMAIN: &[u8] = b"hsk-studio-score/chain-identity@1";

/// A whole clip to render: planar samples on one timeline, rendered in blocks of `block_frames`.
/// `layout` describes the INPUT channels; the OUTPUT channels (sink chunks, receipt labels, meters
/// and digest) follow the layout of the caller's [`OutputBlock`], which the processor must accept
/// together with `layout`.
#[derive(Clone, Copy)]
pub struct BounceRequest<'a> {
    pub sample_rate_hz: u32,
    pub layout: &'a ChannelLayout,
    pub start_sample: u64,
    pub channels: &'a [&'a [f32]],
    pub block_frames: usize,
}

/// A run of retained output frames handed to the sink, one slice per OUTPUT channel.
/// `start_sample` is on the INPUT timeline (latency already compensated), so the first chunk
/// starts at `BounceRequest::start_sample`.
pub struct BounceChunk<'a> {
    pub start_sample: u64,
    pub channels: &'a [&'a [f32]],
}

/// Exact accounting of one bounce (STU-ARC-021): requested versus actual frame counts, exact
/// ticks, whole-clip meters, revision, processor-chain identity and a deterministic sample digest.
///
/// * `requested_frames` = input frames; `actual_frames` = frames delivered to the sink; the bounce
///   renders `latency` extra silent frames through the processor and drops the first `latency`
///   output frames, so both are equal unless something is wrong, and neither is clamped.
/// * `digest` = `SHA-256(DOMAIN || channels u16 LE || for each channel in OUTPUT layout order:
///   label u16 LE || frames u64 LE || SHA-256(that channel's retained samples as f32 LE bytes))`
///   with `DOMAIN = "hsk-studio-score/bounce-digest@1"`; it does not depend on the block size.
///   `channels`, `labels()`, `peak()` and `rms()` describe the output layout, which equals the
///   input layout for layout-preserving chains.
/// * `chain_identity` = `SHA-256("hsk-studio-score/chain-identity@1" || crate version || 0x00 ||
///   Processor::identity())`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BounceReceipt {
    pub requested_frames: u64,
    pub actual_frames: u64,
    pub sample_rate_hz: u32,
    pub start_sample: u64,
    pub end_sample: u64,
    pub start_ticks: Ticks,
    pub end_ticks: Ticks,
    pub blocks: u32,
    pub revision: u64,
    pub latency_dropped_frames: u32,
    pub chain_identity: [u8; 32],
    pub digest: [u8; 32],
    channels: usize,
    labels: [u16; MAX_CHANNELS],
    peak: [f32; MAX_CHANNELS],
    rms: [f32; MAX_CHANNELS],
}

impl BounceReceipt {
    pub fn labels(&self) -> &[u16] {
        &self.labels[..self.channels]
    }
    /// Whole-clip absolute peak per channel.
    pub fn peak(&self) -> &[f32] {
        &self.peak[..self.channels]
    }
    /// Whole-clip RMS per channel (f64 accumulation).
    pub fn rms(&self) -> &[f32] {
        &self.rms[..self.channels]
    }
    pub fn duration_ticks(&self) -> u64 {
        self.end_ticks.value() - self.start_ticks.value()
    }
    pub fn digest_hex(&self) -> String {
        crate::digest::to_hex(&self.digest)
    }
    pub fn chain_identity_hex(&self) -> String {
        crate::digest::to_hex(&self.chain_identity)
    }
}

fn identity_digest(identity: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(IDENTITY_DOMAIN);
    hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    hasher.update(&[0]);
    hasher.update(identity.as_bytes());
    hasher.finalize()
}

impl Engine {
    /// Renders a whole clip through `proc` in blocks and returns the exact receipt.
    ///
    /// Everything refusable is checked before the first block runs or the sink is called. The
    /// processor is reset first (a bounce starts from clean state) and again on any failure. The
    /// engine slot is held for the whole bounce; cancel and revision are re-checked between
    /// blocks. Chunks already delivered to `sink` are provisional: only a returned receipt makes
    /// the bounce final.
    pub fn bounce(
        &self,
        ctx: &RequestContext,
        request: &BounceRequest<'_>,
        proc: &mut dyn Processor,
        out: &mut OutputBlock,
        sink: &mut dyn FnMut(BounceChunk<'_>) -> Result<(), ScoreError>,
        cancel: &CancellationToken,
    ) -> Result<BounceReceipt, ScoreError> {
        let _slot = self.admit(ctx, cancel)?;

        let channels = request.layout.channels();
        // Output side: the layout the processor writes, fixed by the caller's output block.
        let out_labels: Vec<u16> = out.layout().labels().to_vec();
        let out_channels = out_labels.len();
        let frames = request.channels.first().map_or(0, |c| c.len());
        if frames == 0 {
            return Err(ScoreError::OutOfRange);
        }
        if request.channels.len() != channels {
            return Err(ScoreError::ChannelMismatch);
        }
        if request.channels.iter().any(|c| c.len() != frames) {
            return Err(ScoreError::LengthMismatch);
        }
        if request.sample_rate_hz == 0 || request.sample_rate_hz > MAX_SAMPLE_RATE_HZ {
            return Err(ScoreError::InvalidRate);
        }
        ticks_per_sample(request.sample_rate_hz)?;
        if proc
            .required_sample_rate_hz()
            .is_some_and(|rate| rate != request.sample_rate_hz)
        {
            return Err(ScoreError::InvalidRate);
        }
        if !proc.accepts(request.layout, out.layout()) {
            return Err(ScoreError::ChannelMismatch);
        }
        let block = request.block_frames;
        if block == 0 {
            return Err(ScoreError::OutOfRange);
        }
        if block > out.capacity()
            || block > MAX_BLOCK_FRAMES
            || proc.max_frames().is_some_and(|max| block > max)
        {
            return Err(ScoreError::CapacityExceeded);
        }
        if request
            .channels
            .iter()
            .any(|c| c.iter().any(|s| !s.is_finite()))
        {
            return Err(ScoreError::NotFinite);
        }
        let latency = proc.latency_samples() as usize;
        let frames_u64 = u64::try_from(frames).map_err(|_| ScoreError::PositionOverflow)?;
        let total = frames
            .checked_add(latency)
            .ok_or(ScoreError::PositionOverflow)?;
        let end_sample = request
            .start_sample
            .checked_add(frames_u64)
            .ok_or(ScoreError::PositionOverflow)?;
        request
            .start_sample
            .checked_add(u64::try_from(total).map_err(|_| ScoreError::PositionOverflow)?)
            .ok_or(ScoreError::PositionOverflow)?;
        let start_ticks = sample_to_ticks(request.start_sample, request.sample_rate_hz)?;
        let end_ticks = sample_to_ticks(end_sample, request.sample_rate_hz)?;

        proc.reset();
        let mut run = BounceRun {
            digests: vec![Sha256::new(); out_channels],
            peak: [0.0; MAX_CHANNELS],
            energy: [0.0; MAX_CHANNELS],
            actual: 0,
            blocks: 0,
        };
        let outcome = self.bounce_blocks(
            ctx, request, proc, out, sink, cancel, frames, total, latency, &mut run,
        );
        if let Err(error) = outcome {
            proc.reset();
            return Err(error);
        }

        let mut channel_digests = Sha256::new();
        channel_digests.update(DIGEST_DOMAIN);
        channel_digests.update(&(out_channels as u16).to_le_bytes());
        let mut receipt = BounceReceipt {
            requested_frames: frames_u64,
            actual_frames: run.actual,
            sample_rate_hz: request.sample_rate_hz,
            start_sample: request.start_sample,
            end_sample,
            start_ticks,
            end_ticks,
            blocks: run.blocks,
            revision: ctx.expected_revision,
            latency_dropped_frames: proc.latency_samples(),
            chain_identity: identity_digest(&proc.identity()),
            digest: [0; 32],
            channels: out_channels,
            labels: [0; MAX_CHANNELS],
            peak: run.peak,
            rms: [0.0; MAX_CHANNELS],
        };
        for (ch, (hasher, label)) in run.digests.into_iter().zip(&out_labels).enumerate() {
            receipt.labels[ch] = *label;
            receipt.rms[ch] = (run.energy[ch] / run.actual.max(1) as f64).sqrt() as f32;
            channel_digests.update(&label.to_le_bytes());
            channel_digests.update(&run.actual.to_le_bytes());
            channel_digests.update(&hasher.finalize());
        }
        receipt.digest = channel_digests.finalize();
        Ok(receipt)
    }

    #[allow(clippy::too_many_arguments)]
    fn bounce_blocks(
        &self,
        ctx: &RequestContext,
        request: &BounceRequest<'_>,
        proc: &mut dyn Processor,
        out: &mut OutputBlock,
        sink: &mut dyn FnMut(BounceChunk<'_>) -> Result<(), ScoreError>,
        cancel: &CancellationToken,
        frames: usize,
        total: usize,
        latency: usize,
        run: &mut BounceRun,
    ) -> Result<(), ScoreError> {
        let channels = request.layout.channels();
        let out_channels = out.layout().channels();
        let block = request.block_frames;
        let mut stage = vec![0.0_f32; channels * block];
        let mut spec = BlockSpec {
            sample_rate_hz: request.sample_rate_hz,
            layout: request.layout.clone(),
            frames: block,
            start_sample: request.start_sample,
        };
        let mut at = 0;
        while at < total {
            self.settle(ctx, cancel)?;
            let len = block.min(total - at);
            for (dst, src) in stage.chunks_mut(block).zip(request.channels) {
                for (i, slot) in dst[..len].iter_mut().enumerate() {
                    *slot = if at + i < frames { src[at + i] } else { 0.0 };
                }
            }
            spec.frames = len;
            spec.start_sample = request.start_sample + at as u64;
            let mut views: [&[f32]; MAX_CHANNELS] = [&[]; MAX_CHANNELS];
            for (slot, chunk) in views.iter_mut().zip(stage.chunks(block)) {
                *slot = &chunk[..len];
            }
            let input = InputBlock::new(&spec, &views[..channels]);
            self.run_block(ctx, &input, proc, out, cancel)?;
            run.blocks += 1;

            let (lo, hi) = (at.max(latency), (at + len).min(latency + frames));
            if lo < hi {
                let mut kept: [&[f32]; MAX_CHANNELS] = [&[]; MAX_CHANNELS];
                for (ch, slot) in kept.iter_mut().enumerate().take(out_channels) {
                    let samples = out.published_channel(ch).ok_or(ScoreError::ChannelMismatch)?;
                    *slot = &samples[lo - at..hi - at];
                }
                for (ch, samples) in kept[..out_channels].iter().enumerate() {
                    for sample in *samples {
                        run.digests[ch].update(&sample.to_le_bytes());
                        run.peak[ch] = run.peak[ch].max(sample.abs());
                        run.energy[ch] += f64::from(*sample) * f64::from(*sample);
                    }
                }
                sink(BounceChunk {
                    start_sample: request.start_sample + (lo - latency) as u64,
                    channels: &kept[..out_channels],
                })?;
                run.actual += (hi - lo) as u64;
            }
            at += len;
        }
        Ok(())
    }
}

struct BounceRun {
    digests: Vec<Sha256>,
    peak: [f32; MAX_CHANNELS],
    energy: [f64; MAX_CHANNELS],
    actual: u64,
    blocks: u32,
}
