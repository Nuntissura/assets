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
/// stays exact in ticks.
///
/// Exact alignment. With ratio `r = p / q` (reduced) rubato's own startup trim leaves the output
/// ahead of the input by `1 - frac(SINC_LEN / 2 * r)` output frames (derived from rubato 5.0.1's
/// `last_index = -(SINC_LEN - 1)` start and `floor(SINC_LEN * r / 2)` trim, and confirmed on four
/// rate pairs). This type removes that error by construction: it prepends `pad_input_frames`
/// zero frames (the smallest `s` with `q | (SINC_LEN / 2 + s)` that makes the lag a whole,
/// non-negative number of output frames) and drops `drop_output_frames` output frames, so output
/// frame `m` is the band-limited input value at input time `m / r`, and exactly
/// `ceil(frames * r)` frames are produced. Measured residual offset on impulses (five rate pairs,
/// including 48k to 44.1k, 44.1k to 48k, 32k to 48k, x2 and identity): 0.007 to 0.010 output
/// frames, about the 1/128 sinc-table step, versus 0.4 to 1.0 frames for rubato's own trim.
pub struct ClipResampler {
    inner: Async<f32>,
    input_rate_hz: u32,
    output_rate_hz: u32,
    channels: usize,
    chunk_frames: usize,
    pad_input_frames: usize,
    drop_output_frames: usize,
    padded: Vec<f32>,
}

impl std::fmt::Debug for ClipResampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipResampler")
            .field("input_rate_hz", &self.input_rate_hz)
            .field("output_rate_hz", &self.output_rate_hz)
            .field("channels", &self.channels)
            .field("pad_input_frames", &self.pad_input_frames)
            .field("drop_output_frames", &self.drop_output_frames)
            .finish_non_exhaustive()
    }
}

/// Result of one resampled clip: the output block receipt plus exact frame accounting.
/// `block.processed_frames` is the ACTUAL output frame count and equals
/// `requested_output_frames = ceil(input_frames * output_rate / input_rate)`, computed in integer
/// arithmetic (u128), never from rubato's f64 ratio (which can round a mathematically exact count
/// up by one frame, e.g. 44100 frames at 44.1 kHz to 48 kHz). Frames rubato produced beyond that
/// count are cut off after the alignment drop, zeroed in the output buffer, and reported in
/// `trimmed_tail_frames`; a producer that falls short of the exact count is refused
/// (`ResampleFailed`), never zero-padded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResampleReceipt {
    pub block: BlockReceipt,
    pub input_frames: usize,
    pub input_rate_hz: u32,
    pub requested_output_frames: usize,
    /// Frames rubato trimmed itself (`floor(SINC_LEN * ratio / 2)`).
    pub delay_trimmed_frames: usize,
    /// Zero frames prepended to the input for exact alignment.
    pub pad_input_frames: usize,
    /// Output frames dropped after rubato's trim for exact alignment.
    pub drop_output_frames: usize,
    /// Frames rubato produced past `drop_output_frames + requested_output_frames`; trimmed so the
    /// published count is exactly `requested_output_frames`.
    pub trimmed_tail_frames: usize,
}

impl ResampleReceipt {
    pub fn actual_output_frames(&self) -> usize {
        self.block.processed_frames
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// `(pad_input_frames, drop_output_frames)` for ratio `p / q`; see [`ClipResampler`].
fn alignment(p: u64, q: u64) -> Result<(usize, usize), ScoreError> {
    let half = (SINC_LEN / 2) as u128;
    let (p, q) = (u128::from(p), u128::from(q));
    let floor_delay = half * p / q;
    // Candidates are the pads that complete (half + s) to a multiple of q.
    let mut s = (q - half % q) % q;
    for _ in 0..4 {
        let total = (half + s) * p / q;
        if total > floor_delay {
            let drop = total - 1 - floor_delay;
            return match (usize::try_from(s), usize::try_from(drop)) {
                (Ok(s), Ok(drop)) if s <= MAX_BLOCK_FRAMES => Ok((s, drop)),
                _ => Err(ScoreError::InvalidRate),
            };
        }
        s += q;
    }
    Err(ScoreError::InvalidRate)
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
        let g = gcd(u64::from(input_rate_hz), u64::from(output_rate_hz));
        let (pad_input_frames, drop_output_frames) = alignment(
            u64::from(output_rate_hz) / g,
            u64::from(input_rate_hz) / g,
        )?;
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
            chunk_frames,
            pad_input_frames,
            drop_output_frames,
            padded: Vec::new(),
        })
    }
    pub fn input_rate_hz(&self) -> u32 {
        self.input_rate_hz
    }
    pub fn output_rate_hz(&self) -> u32 {
        self.output_rate_hz
    }
    pub fn pad_input_frames(&self) -> usize {
        self.pad_input_frames
    }
    pub fn drop_output_frames(&self) -> usize {
        self.drop_output_frames
    }
    pub fn channels(&self) -> usize {
        self.channels
    }
    /// Internal rubato input chunk size this resampler was built with.
    pub fn chunk_frames(&self) -> usize {
        self.chunk_frames
    }
    /// Canonical description of this converter's configuration (kind, rates, channels, chunk size
    /// and the alignment pad/drop it derived). Bounce receipts hash it into a graph identity.
    pub fn identity(&self) -> String {
        format!(
            "clip_resampler:sinc_len={SINC_LEN};window=blackman_harris2;in={};out={};channels={};chunk={};pad={};drop={}",
            self.input_rate_hz,
            self.output_rate_hz,
            self.channels,
            self.chunk_frames,
            self.pad_input_frames,
            self.drop_output_frames
        )
    }
    /// Exact output frame count for a clip: `ceil(input_frames * output_rate / input_rate)`.
    pub fn output_frames_for(&self, input_frames: usize) -> Option<usize> {
        requested_frames(input_frames, self.input_rate_hz, self.output_rate_hz)
    }
    /// Output capacity (frames) a clip of `input_frames` needs.
    pub fn output_capacity_for(&mut self, input_frames: usize) -> usize {
        let padded = self
            .inner
            .process_all_needed_output_len(input_frames + self.pad_input_frames);
        let aligned = self
            .output_frames_for(input_frames)
            .map_or(usize::MAX, |frames| frames + self.drop_output_frames);
        padded.max(aligned)
    }
}

/// `ceil(frames * out / in)` in u128; `None` when it does not fit `usize`.
pub(crate) fn requested_frames(
    frames: usize,
    input_rate_hz: u32,
    output_rate_hz: u32,
) -> Option<usize> {
    let numerator = u128::try_from(frames).ok()? * u128::from(output_rate_hz);
    let denominator = u128::from(input_rate_hz);
    usize::try_from(numerator.div_ceil(denominator)).ok()
}

/// Maps an input-timeline sample to the output timeline; refuses positions that are not
/// representable exactly at the output rate (no silent rounding).
pub(crate) fn map_start(
    sample: u64,
    input_rate_hz: u32,
    output_rate_hz: u32,
) -> Result<u64, ScoreError> {
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
        self.resample_held(ctx, input, resampler, out, cancel)
    }

    /// Body of [`Engine::resample`]; the caller already holds the engine slot (also the SRC stage
    /// of [`Engine::bounce_resampled`]).
    pub(crate) fn resample_held(
        &self,
        ctx: &RequestContext,
        input: &InputBlock<'_>,
        resampler: &mut ClipResampler,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
    ) -> Result<ResampleReceipt, ScoreError> {
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
        let (pad, drop) = (resampler.pad_input_frames, resampler.drop_output_frames);
        let padded_frames = spec.frames + pad;
        let needed = resampler
            .inner
            .process_all_needed_output_len(padded_frames)
            .max(requested + drop);
        if needed > out.capacity() {
            return Err(ScoreError::CapacityExceeded);
        }

        out.unpublish();
        resampler.inner.reset();
        let capacity = out.capacity();
        let channels = resampler.channels;
        if pad > 0 {
            resampler.padded.clear();
            resampler.padded.resize(channels * padded_frames, 0.0);
            for (ch, samples) in input.channels().iter().enumerate() {
                let start = ch * padded_frames + pad;
                resampler.padded[start..start + spec.frames].copy_from_slice(samples);
            }
        }
        let produced = {
            let views: Vec<&[f32]> = if pad > 0 {
                resampler.padded.chunks(padded_frames).collect()
            } else {
                input.channels().to_vec()
            };
            let input_adapter = SequentialSliceOfSlices::new(&views, channels, padded_frames)
                .map_err(|_| ScoreError::ResampleFailed)?;
            let mut output_adapter = SequentialSlice::new_mut(out.backing_mut(), channels, capacity)
                .map_err(|_| ScoreError::ResampleFailed)?;
            resampler.inner.process_all_into_buffer(
                &input_adapter,
                &mut output_adapter,
                padded_frames,
                None,
            )
        };
        // `produced` is rubato's valid-frame count after its own startup trim. The published
        // count is the exact integer `requested`; any excess is the recorded tail.
        let settled = produced
            .map_err(|_| ScoreError::ResampleFailed)
            .and_then(|(_, frames)| {
                if frames < requested + drop || frames > capacity {
                    Err(ScoreError::ResampleFailed)
                } else {
                    self.settle(ctx, cancel).map(|()| frames)
                }
            });
        let rubato_frames = match settled {
            Ok(frames) => frames,
            Err(error) => {
                resampler.inner.reset();
                return Err(error);
            }
        };
        let trimmed_tail_frames = rubato_frames - requested - drop;
        {
            let backing = out.backing_mut();
            for ch in 0..channels {
                let base = ch * capacity;
                if drop > 0 {
                    backing.copy_within(base + drop..base + drop + requested, base);
                }
                // Nothing past the published count survives: the shifted-over originals, the
                // trimmed tail and anything rubato wrote beyond its reported count are zeroed.
                backing[base + requested..base + capacity].fill(0.0);
            }
        }
        let end_sample = u64::try_from(requested)
            .ok()
            .and_then(|frames| out_start.checked_add(frames))
            .ok_or(ScoreError::PositionOverflow)?;
        let block = BlockReceipt::measure(
            out,
            requested,
            0,
            resampler.output_rate_hz,
            out_start,
            end_sample,
            ctx.expected_revision,
        );
        let delay_trimmed_frames = resampler.inner.output_delay();
        resampler.inner.reset();
        out.publish(requested, out_start, resampler.output_rate_hz);
        Ok(ResampleReceipt {
            block,
            input_frames: spec.frames,
            input_rate_hz: spec.sample_rate_hz,
            requested_output_frames: requested,
            delay_trimmed_frames,
            pad_input_frames: pad,
            drop_output_frames: drop,
            trimmed_tail_frames,
        })
    }
}
