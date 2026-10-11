use crate::block::{InputBlock, OutputBlock};
use crate::dsp::poll;
use crate::engine::{BlockReceipt, Engine, RequestContext};
use crate::error::ScoreError;
use crate::layout::CANCEL_CHECK_FRAMES;
use hsk_studio_accord::CancellationToken;

/// Widest send list accepted (authored bound).
pub const MAX_SENDS: usize = 256;

/// One routing from a track block to a submix with its own linear level (STU-FX-137).
#[derive(Clone, Copy)]
pub struct SendRoute<'a> {
    pub input: InputBlock<'a>,
    pub level: f32,
}

impl Engine {
    /// Sums an ordered send list into a submix block: `out = sum_i level_i * send_i`, accumulated
    /// in send order (deterministic). Every send must share the submix layout, rate, start sample
    /// and frame count. Same admission, cancel, revision and publication rules as `process`;
    /// nothing is written before every send validated.
    pub fn mix(
        &self,
        ctx: &RequestContext,
        sends: &[SendRoute<'_>],
        out: &mut OutputBlock,
        cancel: &CancellationToken,
    ) -> Result<BlockReceipt, ScoreError> {
        let _slot = self.admit(ctx, cancel)?;

        let first = sends.first().ok_or(ScoreError::OutOfRange)?;
        if sends.len() > MAX_SENDS {
            return Err(ScoreError::CapacityExceeded);
        }
        let spec = first.input.spec();
        for send in sends {
            if !send.level.is_finite() {
                return Err(ScoreError::NotFinite);
            }
            send.input.validate()?;
            let other = send.input.spec();
            if other.layout != *out.layout() {
                return Err(ScoreError::ChannelMismatch);
            }
            if other.sample_rate_hz != spec.sample_rate_hz
                || other.start_sample != spec.start_sample
                || other.frames != spec.frames
            {
                return Err(ScoreError::TimelineMismatch);
            }
        }
        if spec.frames > out.capacity() {
            return Err(ScoreError::CapacityExceeded);
        }
        let end_sample = spec.end_sample()?;

        out.unpublish();
        let frames = spec.frames;
        let mut written = Ok(());
        'channels: for (ch, dst) in out.channels_mut().enumerate() {
            let mut at = 0;
            while at < frames {
                if let Err(error) = poll(cancel) {
                    written = Err(error);
                    break 'channels;
                }
                let end = (at + CANCEL_CHECK_FRAMES).min(frames);
                for (k, d) in dst[at..end].iter_mut().enumerate() {
                    let mut acc = 0.0_f32;
                    for send in sends {
                        acc += send.level * send.input.channel(ch)[at + k];
                    }
                    *d = acc;
                }
                at = end;
            }
        }
        written.and_then(|()| self.settle(ctx, cancel))?;

        let receipt = BlockReceipt::measure(
            out,
            frames,
            0,
            spec.sample_rate_hz,
            spec.start_sample,
            end_sample,
            ctx.expected_revision,
        );
        out.publish(frames, spec.start_sample, spec.sample_rate_hz);
        Ok(receipt)
    }
}
