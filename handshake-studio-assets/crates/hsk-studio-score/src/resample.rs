use crate::block::{InputBlock, OutputBlock};
use crate::engine::{BlockReceipt, Engine, RequestContext};
use crate::error::ScoreError;
use crate::layout::{MAX_BLOCK_FRAMES, MAX_CHANNELS, ticks_per_sample};
use hsk_studio_accord::CancellationToken;
use rubato::audioadapter_buffers::direct::{SequentialSlice, SequentialSliceOfSlices};
use rubato::{Async, FixedAsync, Resampler, SincInterpolationParameters, WindowFunction};

/// Sinc kernel length (rubato's recommended `SincInterpolationParameters::new(256, ..)`).
pub const SINC_LEN: usize = 256;
/// Smallest accepted internal chunk (authored bound).
pub const MIN_CHUNK_FRAMES: usize = 64;

/// Fixed-ratio band-limited sample-rate converter for whole clips (rubato asynchronous sinc,
/// `f32`, fixed input chunk). Both rates must divide `TICKS_PER_SECOND` so the output timeline
/// stays exact in ticks. The startup delay of the filter is trimmed, so the output is aligned
/// with the input (`delay_trimmed_frames` reports how much was removed).
pub struct ClipResampler {
    inner: Async<f32>,
    input_rate_hz: u32,
    output_rate_hz: u32,
    channels: usize,
}

impl std::fmt::Debug for ClipResampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipResampler")
            .field("input_rate_hz", &self.input_rate_hz)
            .field("output_rate_hz", &self.output_rate_hz)
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

/// Result of one resampled clip: the output block receipt plus exact frame accounting.
/// `block.processed_frames` is the ACTUAL output frame count; `requested_output_frames` is
/// `ceil(input_frames * output_rate / input_rate)`; neither is clamped or truncated silently.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResampleReceipt {
    pub block: BlockReceipt,
    pub input_frames: usize,
    pub input_rate_hz: u32,
    pub requested_output_frames: usize,
    pub delay_trimmed_frames: usize,
}

impl ResampleReceipt {
    pub fn actual_output_frames(&self) -> usize {
        self.block.processed_frames
    }
}

impl ClipResampler {
    pub fn new(
        input_rate_hz: u32,
        output_rate_hz: u32,
        channels: usize,
        chunk_frames: usize,
    ) -> Result<Self, ScoreError> {
        ticks_per_sample(input_rate_hz)?;
        ticks_per_sample(output_rate_hz)?;
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(ScoreError::InvalidLayout);
        }
        if !(MIN_CHUNK_FRAMES..=MAX_BLOCK_FRAMES).contains(&chunk_frames) {
            return Err(ScoreError::OutOfRange);
        }
        let ratio = f64::from(output_rate_hz) / f64::from(input_rate_hz);
        let parameters = SincInterpolationParameters::new(SINC_LEN, WindowFunction::BlackmanHarris2);
        let inner = Async::<f32>::new_sinc(
            ratio,
            1.0,
            &parameters,
            chunk_frames,
            channels,
            FixedAsync::Input,
        )
        .map_err(|_| ScoreError::ResampleFailed)?;
        Ok(Self {
            inner,
            input_rate_hz,
            output_rate_hz,
            channels,
        })
    }
    pub fn input_rate_hz(&self) -> u32 {
        self.input_rate_hz
    }
    pub fn output_rate_hz(&self) -> u32 {
        self.output_rate_hz
    }
    /// Output capacity (frames) a clip of `input_frames` needs.
    pub fn output_capacity_for(&mut self, input_frames: usize) -> usize {
        self.inner.process_all_needed_output_len(input_frames)
    }
}

/// `ceil(frames * out / in)` in u128; `None` when it does not fit `usize`.
fn requested_frames(frames: usize, input_rate_hz: u32, output_rate_hz: u32) -> Option<usize> {
    let numerator = u128::try_from(frames).ok()? * u128::from(output_rate_hz);
    let denominator = u128::from(input_rate_hz);
    usize::try_from(numerator.div_ceil(denominator)).ok()
}

/// Maps an input-timeline sample to the output timeline; refuses positions that are not
/// representable exactly at the output rate (no silent rounding).
fn map_start(sample: u64, input_rate_hz: u32, output_rate_hz: u32) -> Result<u64, ScoreError> {
    let scaled = u128::from(sample) * u128::from(output_rate_hz);
    let rate = u128::from(input_rate_hz);
    if !scaled.is_multiple_of(rate) {
        return Err(ScoreError::OutOfRange);
    }
    u64::try_from(scaled / rate).map_err(|_| ScoreError::PositionOverflow)
}

impl Engine {
    /// Resamples one admitted clip into `out` with the same admission, cancel, revision and
    /// publication rules as [`Engine::process`]. The output block is published at
    /// `start_sample * output_rate / input_rate` (must be exact) in the output timeline.
    pub fn resample(
        &self,
        ctx: &RequestContext,
        input: &InputBlock<'_>,
        resampler: &mut ClipResampler,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
    ) -> Result<ResampleReceipt, ScoreError> {
        let _slot = self.admit(ctx, cancel)?;

        input.validate()?;
        let spec = input.spec();
        if spec.sample_rate_hz != resampler.input_rate_hz {
            return Err(ScoreError::InvalidRate);
        }
        if spec.layout.channels() != resampler.channels
            || out.layout().channels() != resampler.channels
        {
            return Err(ScoreError::ChannelMismatch);
        }
        let out_start = map_start(
            spec.start_sample,
            resampler.input_rate_hz,
            resampler.output_rate_hz,
        )?;
        let requested = requested_frames(spec.frames, resampler.input_rate_hz, resampler.output_rate_hz)
            .ok_or(ScoreError::CapacityExceeded)?;
        let needed = resampler.inner.process_all_needed_output_len(spec.frames);
        if needed > out.capacity() || requested > out.capacity() {
            return Err(ScoreError::CapacityExceeded);
        }

        out.unpublish();
        resampler.inner.reset();
        let capacity = out.capacity();
        let written = {
            let input_adapter = SequentialSliceOfSlices::new(
                input.channels(),
                resampler.channels,
                spec.frames,
            )
            .map_err(|_| ScoreError::ResampleFailed)?;
            let mut output_adapter =
                SequentialSlice::new_mut(out.backing_mut(), resampler.channels, capacity)
                    .map_err(|_| ScoreError::ResampleFailed)?;
            resampler.inner.process_all_into_buffer(
                &input_adapter,
                &mut output_adapter,
                spec.frames,
                None,
            )
        };
        let settled = written
            .map_err(|_| ScoreError::ResampleFailed)
            .and_then(|(_, frames)| self.settle(ctx, cancel).map(|()| frames));
        let written = match settled {
            Ok(frames) => frames,
            Err(error) => {
                resampler.inner.reset();
                return Err(error);
            }
        };
        let end_sample = u64::try_from(written)
            .ok()
            .and_then(|frames| out_start.checked_add(frames))
            .ok_or(ScoreError::PositionOverflow)?;
        let block = BlockReceipt::measure(
            out,
            written,
            0,
            resampler.output_rate_hz,
            out_start,
            end_sample,
            ctx.expected_revision,
        );
        let delay_trimmed_frames = resampler.inner.output_delay();
        resampler.inner.reset();
        out.publish(written, out_start, resampler.output_rate_hz);
        Ok(ResampleReceipt {
            block,
            input_frames: spec.frames,
            input_rate_hz: spec.sample_rate_hz,
            requested_output_frames: requested,
            delay_trimmed_frames,
        })
    }
}
