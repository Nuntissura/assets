//! Block-streaming use of [`ClipResampler`]: the same converter, the same alignment and the same
//! rubato call sequence as the whole-clip path, fed in arbitrary pieces so a clip is no longer
//! limited by `MAX_BLOCK_FRAMES`.
//!
//! Exactness argument. [`Engine::resample`](crate::Engine::resample) hands rubato the clip
//! (`pad_input_frames` zero frames first), cut in fixed chunks of `chunk_frames`, then pumps
//! silence, and keeps `requested` frames after skipping rubato's startup delay and
//! `drop_output_frames`. The streaming path performs the identical `process_into_buffer` sequence
//! (a chunk is processed as soon as it is full; the last partial chunk is processed with
//! `partial_len`; silence is pumped with `partial_len = Some(0)`), so every output frame is
//! produced by the same arithmetic on the same state: the delivered frames are bit-identical.
//! Memory: rubato's own history plus `stream_in` (`channels * chunk_frames`) and `stream_out`
//! (`channels * output_frames_max`), both sized by [`ClipResampler::begin_stream`]; a push or a
//! finish allocates nothing here.
use crate::block::all_finite;
use crate::error::ScoreError;
use crate::layout::MAX_CHANNELS;
use crate::resample::{ClipResampler, requested_frames};
use rubato::audioadapter_buffers::direct::SequentialSlice;
use rubato::{Indexing, Resampler};

/// Upper bound on silence chunks pumped after the last input (a few are needed in practice).
const MAX_FLUSH_PUMPS: usize = 4096;

/// Receiver of converted frames: one slice per channel, all the same length.
pub type StreamEmit<'a> = &'a mut dyn FnMut(&[&[f32]]) -> Result<(), ScoreError>;

#[derive(Default)]
pub(crate) struct StreamState {
    active: bool,
    declared: usize,
    fed: usize,
    pad_left: usize,
    staged: usize,
    to_skip: usize,
    requested: usize,
    remaining: usize,
    expected_raw: usize,
    trim: usize,
    chunks: u64,
    peak: [f32; MAX_CHANNELS],
    energy: [f64; MAX_CHANNELS],
}

/// Exact accounting of one streamed conversion; the counts mirror
/// [`ResampleReceipt`](crate::ResampleReceipt).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreamReceipt {
    pub input_frames: usize,
    /// `ceil(input_frames * out / in)` in integer arithmetic.
    pub requested_output_frames: usize,
    /// Frames handed to the receiver; equals `requested_output_frames` or the call fails.
    pub delivered_frames: usize,
    /// Frames rubato trimmed itself (`floor(SINC_LEN * ratio / 2)`).
    pub delay_trimmed_frames: usize,
    pub pad_input_frames: usize,
    pub drop_output_frames: usize,
    /// Frames rubato would have produced past `drop + requested` for this clip (the whole-clip
    /// path's trimmed tail); never delivered.
    pub trimmed_tail_frames: usize,
    /// rubato `process_into_buffer` calls made.
    pub chunks_processed: u64,
    /// Bytes of streaming buffers held by the converter (excludes rubato's internal history).
    pub buffer_bytes: usize,
    channels: usize,
    peak: [f32; MAX_CHANNELS],
    rms: [f32; MAX_CHANNELS],
}

impl StreamReceipt {
    /// Absolute peak per channel of the delivered frames.
    pub fn peak(&self) -> &[f32] {
        &self.peak[..self.channels]
    }
    /// RMS per channel of the delivered frames (f64 accumulation).
    pub fn rms(&self) -> &[f32] {
        &self.rms[..self.channels]
    }
}

impl ClipResampler {
    /// Bytes held by the streaming buffers (0 until the first [`begin_stream`](Self::begin_stream)).
    pub fn stream_buffer_bytes(&self) -> usize {
        (self.stream_in.len() + self.stream_out.len()) * std::mem::size_of::<f32>()
    }

    /// Starts a streamed conversion of a clip of exactly `input_frames` frames. Resets the
    /// converter, sizes the streaming buffers (the only allocation of the stream) and refuses
    /// everything decidable up front: zero frames, a count that does not fit, and a converter
    /// that could not produce the requested count (`ResampleFailed`).
    pub fn begin_stream(&mut self, input_frames: usize) -> Result<(), ScoreError> {
        self.stream.active = false;
        if input_frames == 0 {
            return Err(ScoreError::OutOfRange);
        }
        let requested = requested_frames(input_frames, self.input_rate_hz(), self.output_rate_hz())
            .ok_or(ScoreError::CapacityExceeded)?;
        let padded = input_frames
            .checked_add(self.pad_input_frames)
            .ok_or(ScoreError::PositionOverflow)?;
        // The same expression rubato's `process_all_into_buffer` uses for its output length.
        let expected_raw = (self.inner.resample_ratio() * padded as f64).ceil() as usize;
        let needed = requested
            .checked_add(self.drop_output_frames)
            .ok_or(ScoreError::PositionOverflow)?;
        if expected_raw < needed {
            return Err(ScoreError::ResampleFailed);
        }
        self.inner.reset();
        let out_max = self.inner.output_frames_max();
        self.stream_in.clear();
        self.stream_in
            .resize(self.channels * self.chunk_frames, 0.0);
        self.stream_out.clear();
        self.stream_out.resize(self.channels * out_max, 0.0);
        let trim = self.inner.output_delay();
        self.stream = StreamState {
            active: true,
            declared: input_frames,
            pad_left: self.pad_input_frames,
            to_skip: trim + self.drop_output_frames,
            requested,
            remaining: requested,
            expected_raw,
            trim,
            ..StreamState::default()
        };
        Ok(())
    }

    /// Ends a stream early and returns the converter to its initial state.
    pub fn abort_stream(&mut self) {
        self.stream.active = false;
        self.inner.reset();
    }

    /// Feeds the next frames of the clip (any length, one slice per channel) and passes every
    /// converted frame that becomes available to `emit`. Refuses non-finite samples and frames
    /// beyond the declared count. Any error ends the stream (`abort_stream`).
    pub fn push_stream(
        &mut self,
        input: &[&[f32]],
        emit: StreamEmit<'_>,
    ) -> Result<(), ScoreError> {
        let result = self.push_checked(input, emit);
        if result.is_err() {
            self.abort_stream();
        }
        result
    }

    /// Flushes the last partial chunk and the converter delay, then returns the exact receipt.
    /// The declared number of frames must have been pushed; any error ends the stream.
    pub fn finish_stream(&mut self, emit: StreamEmit<'_>) -> Result<StreamReceipt, ScoreError> {
        let result = self.finish_checked(emit);
        self.stream.active = false;
        self.inner.reset();
        result
    }

    fn push_checked(&mut self, input: &[&[f32]], emit: StreamEmit<'_>) -> Result<(), ScoreError> {
        if !self.stream.active {
            return Err(ScoreError::OutOfRange);
        }
        if input.len() != self.channels {
            return Err(ScoreError::ChannelMismatch);
        }
        let frames = input.first().map_or(0, |c| c.len());
        if input.iter().any(|c| c.len() != frames) {
            return Err(ScoreError::LengthMismatch);
        }
        if frames == 0 {
            return Ok(());
        }
        if frames > self.stream.declared - self.stream.fed {
            return Err(ScoreError::LengthMismatch);
        }
        if input.iter().any(|c| c.iter().any(|s| !s.is_finite())) {
            return Err(ScoreError::NotFinite);
        }
        self.feed_pad(emit)?;
        self.feed(frames, Some(input), emit)?;
        self.stream.fed += frames;
        Ok(())
    }

    fn finish_checked(&mut self, emit: StreamEmit<'_>) -> Result<StreamReceipt, ScoreError> {
        if !self.stream.active {
            return Err(ScoreError::OutOfRange);
        }
        if self.stream.fed != self.stream.declared {
            return Err(ScoreError::LengthMismatch);
        }
        self.feed_pad(emit)?;
        if self.stream.staged > 0 {
            let staged = std::mem::take(&mut self.stream.staged);
            self.chunk(Some(staged), emit)?;
        }
        let mut pumps = 0;
        while self.stream.remaining > 0 {
            if pumps >= MAX_FLUSH_PUMPS {
                return Err(ScoreError::ResampleFailed);
            }
            pumps += 1;
            self.chunk(Some(0), emit)?;
        }
        let state = &self.stream;
        let delivered = state.requested - state.remaining;
        let mut receipt = StreamReceipt {
            input_frames: state.declared,
            requested_output_frames: state.requested,
            delivered_frames: delivered,
            delay_trimmed_frames: state.trim,
            pad_input_frames: self.pad_input_frames,
            drop_output_frames: self.drop_output_frames,
            trimmed_tail_frames: state.expected_raw - state.requested - self.drop_output_frames,
            chunks_processed: state.chunks,
            buffer_bytes: self.stream_buffer_bytes(),
            channels: self.channels,
            peak: state.peak,
            rms: [0.0; MAX_CHANNELS],
        };
        for ch in 0..self.channels {
            receipt.rms[ch] = (state.energy[ch] / delivered.max(1) as f64).sqrt() as f32;
        }
        Ok(receipt)
    }

    fn feed_pad(&mut self, emit: StreamEmit<'_>) -> Result<(), ScoreError> {
        let pad = std::mem::take(&mut self.stream.pad_left);
        if pad > 0 {
            self.feed(pad, None, emit)?;
        }
        Ok(())
    }

    /// Appends `frames` frames (`input` = data, `None` = silence) to the chunk staging area and
    /// processes every chunk that fills up.
    fn feed(
        &mut self,
        frames: usize,
        input: Option<&[&[f32]]>,
        emit: StreamEmit<'_>,
    ) -> Result<(), ScoreError> {
        let chunk = self.chunk_frames;
        let mut done = 0;
        while done < frames {
            let at = self.stream.staged;
            let take = (chunk - at).min(frames - done);
            for ch in 0..self.channels {
                let dst = &mut self.stream_in[ch * chunk + at..ch * chunk + at + take];
                match input {
                    Some(source) => dst.copy_from_slice(&source[ch][done..done + take]),
                    None => dst.fill(0.0),
                }
            }
            self.stream.staged += take;
            done += take;
            if self.stream.staged == chunk {
                self.stream.staged = 0;
                self.chunk(None, emit)?;
            }
        }
        Ok(())
    }

    /// One rubato call on the staged chunk, then skip/keep bookkeeping and delivery.
    fn chunk(
        &mut self,
        partial_len: Option<usize>,
        emit: StreamEmit<'_>,
    ) -> Result<(), ScoreError> {
        let (channels, chunk) = (self.channels, self.chunk_frames);
        let out_max = self.stream_out.len() / channels;
        let indexing = Indexing {
            input_offset: 0,
            output_offset: 0,
            active_channels_mask: None,
            partial_len,
        };
        let produced = {
            let input = SequentialSlice::new(&self.stream_in[..], channels, chunk)
                .map_err(|_| ScoreError::ResampleFailed)?;
            let mut output = SequentialSlice::new_mut(&mut self.stream_out[..], channels, out_max)
                .map_err(|_| ScoreError::ResampleFailed)?;
            self.inner
                .process_into_buffer(&input, &mut output, Some(&indexing))
                .map_err(|_| ScoreError::ResampleFailed)?
                .1
        };
        self.stream.chunks += 1;
        let skip = self.stream.to_skip.min(produced);
        self.stream.to_skip -= skip;
        let take = (produced - skip).min(self.stream.remaining);
        if take == 0 {
            return Ok(());
        }
        // The sinc sum of finite samples can still exceed `f32::MAX`: a chunk holding a NaN or
        // infinity never reaches the receiver or the meters; the stream ends with `NotFinite`
        // (chunks delivered before it were finite and stay provisional, as for any failure).
        if (0..channels).any(|ch| {
            let base = ch * out_max + skip;
            !all_finite(&self.stream_out[base..base + take])
        }) {
            return Err(ScoreError::NotFinite);
        }
        let mut views: [&[f32]; MAX_CHANNELS] = [&[]; MAX_CHANNELS];
        for (ch, slot) in views.iter_mut().enumerate().take(channels) {
            let base = ch * out_max + skip;
            let samples = &self.stream_out[base..base + take];
            for sample in samples {
                self.stream.peak[ch] = self.stream.peak[ch].max(sample.abs());
                self.stream.energy[ch] += f64::from(*sample) * f64::from(*sample);
            }
            *slot = samples;
        }
        emit(&views[..channels])?;
        self.stream.remaining -= take;
        Ok(())
    }
}
