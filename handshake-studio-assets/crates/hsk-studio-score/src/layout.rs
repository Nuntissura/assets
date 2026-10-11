use crate::error::ScoreError;
use hsk_studio_accord::{TICKS_PER_SECOND, Ticks};

/// Widest layout accepted (the `sixteen_channel` master configuration, STU-FX-137).
pub const MAX_CHANNELS: usize = 16;
/// Authored bound on frames in one block / one output capacity (not a spec value).
pub const MAX_BLOCK_FRAMES: usize = 1 << 20;
/// Authored bound on sample rate (not a spec value); keeps lookahead buffers bounded.
pub const MAX_SAMPLE_RATE_HZ: u32 = 768_000;
/// Studio default sequence sample rate (STU-FX-137).
pub const DEFAULT_SAMPLE_RATE_HZ: u32 = 48_000;
/// Ordered channel labels of the shipped Stereo layout preset (STU-FX-137). The spec declares no
/// labels for Mono or 5.1 (UNVERIFIED); those are caller-supplied custom layouts.
pub const STEREO_LABELS: [u16; 2] = [100, 101];
/// Built-in processors poll cancellation at least once per this many frames.
pub const CANCEL_CHECK_FRAMES: usize = 256;

/// Ordered channel-label list. Pure data: a custom layout is an ordinary instance (STU-FX-137).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelLayout(Vec<u16>);

impl ChannelLayout {
    /// Rejects an empty list, more than [`MAX_CHANNELS`] labels and duplicate labels.
    pub fn new(labels: &[u16]) -> Result<Self, ScoreError> {
        if labels.is_empty() || labels.len() > MAX_CHANNELS {
            return Err(ScoreError::InvalidLayout);
        }
        for (i, label) in labels.iter().enumerate() {
            if labels[..i].contains(label) {
                return Err(ScoreError::InvalidLayout);
            }
        }
        Ok(Self(labels.to_vec()))
    }
    pub fn stereo() -> Self {
        Self(STEREO_LABELS.to_vec())
    }
    /// One channel with a caller-chosen label.
    pub fn mono(label: u16) -> Self {
        Self(vec![label])
    }
    pub fn labels(&self) -> &[u16] {
        &self.0
    }
    pub fn channels(&self) -> usize {
        self.0.len()
    }
}

/// Header of one block: rate, ordered channel labels, frame count and absolute start sample.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockSpec {
    pub sample_rate_hz: u32,
    pub layout: ChannelLayout,
    pub frames: usize,
    pub start_sample: u64,
}

impl BlockSpec {
    /// Exclusive end sample (`start_sample + frames`), checked.
    pub fn end_sample(&self) -> Result<u64, ScoreError> {
        let frames = u64::try_from(self.frames).map_err(|_| ScoreError::PositionOverflow)?;
        self.start_sample
            .checked_add(frames)
            .ok_or(ScoreError::PositionOverflow)
    }
}

/// Exact ticks per sample (STU-VID-012). Rejects rate 0, rates above [`MAX_SAMPLE_RATE_HZ`] and
/// rates that do not divide `TICKS_PER_SECOND`.
pub fn ticks_per_sample(rate_hz: u32) -> Result<u64, ScoreError> {
    let rate = u64::from(rate_hz);
    if rate == 0 || rate_hz > MAX_SAMPLE_RATE_HZ || !TICKS_PER_SECOND.is_multiple_of(rate) {
        return Err(ScoreError::InvalidRate);
    }
    Ok(TICKS_PER_SECOND / rate)
}

pub fn sample_to_ticks(sample: u64, rate_hz: u32) -> Result<Ticks, ScoreError> {
    sample
        .checked_mul(ticks_per_sample(rate_hz)?)
        .map(Ticks::new)
        .ok_or(ScoreError::PositionOverflow)
}

/// Floor conversion: `(sample, remainder_ticks)`; the remainder is explicit, never rounded away.
pub fn ticks_to_sample(ticks: Ticks, rate_hz: u32) -> Result<(u64, u64), ScoreError> {
    let per_sample = ticks_per_sample(rate_hz)?;
    Ok((ticks.value() / per_sample, ticks.value() % per_sample))
}
