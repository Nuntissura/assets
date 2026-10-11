use crate::error::ScoreError;
use crate::layout::{BlockSpec, ChannelLayout, MAX_BLOCK_FRAMES, MAX_SAMPLE_RATE_HZ};

/// Immutable planar PCM view: one `f32` slice per channel, all `spec.frames` long.
/// Construction never fails; [`InputBlock::validate`] is the admission gate.
#[derive(Clone, Copy)]
pub struct InputBlock<'a> {
    spec: &'a BlockSpec,
    channels: &'a [&'a [f32]],
}

impl<'a> InputBlock<'a> {
    pub fn new(spec: &'a BlockSpec, channels: &'a [&'a [f32]]) -> Self {
        Self { spec, channels }
    }
    pub fn spec(&self) -> &'a BlockSpec {
        self.spec
    }
    pub fn frames(&self) -> usize {
        self.spec.frames
    }
    pub fn sample_rate_hz(&self) -> u32 {
        self.spec.sample_rate_hz
    }
    pub fn start_sample(&self) -> u64 {
        self.spec.start_sample
    }
    pub fn layout(&self) -> &'a ChannelLayout {
        &self.spec.layout
    }
    pub fn channels(&self) -> &'a [&'a [f32]] {
        self.channels
    }
    /// One channel's samples. Panics when `ch` is out of range; only call after `validate`.
    pub fn channel(&self, ch: usize) -> &'a [f32] {
        self.channels[ch]
    }

    /// Read-only admission checks; never writes. Order: frames, rate, channel count, slice
    /// lengths, position overflow, then the finite scan over every sample.
    pub fn validate(&self) -> Result<(), ScoreError> {
        let spec = self.spec;
        if spec.frames == 0 {
            return Err(ScoreError::OutOfRange);
        }
        if spec.frames > MAX_BLOCK_FRAMES {
            return Err(ScoreError::CapacityExceeded);
        }
        if spec.sample_rate_hz == 0 || spec.sample_rate_hz > MAX_SAMPLE_RATE_HZ {
            return Err(ScoreError::InvalidRate);
        }
        if self.channels.len() != spec.layout.channels() {
            return Err(ScoreError::ChannelMismatch);
        }
        if self.channels.iter().any(|c| c.len() != spec.frames) {
            return Err(ScoreError::LengthMismatch);
        }
        spec.end_sample()?;
        if self
            .channels
            .iter()
            .any(|c| c.iter().any(|s| !s.is_finite()))
        {
            return Err(ScoreError::NotFinite);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Published {
    frames: usize,
    start_sample: u64,
    sample_rate_hz: u32,
}

/// Caller-owned, preallocated planar output. Only the engine publishes; until then the
/// published view is empty, so a discarded block is never observable.
#[derive(Debug)]
pub struct OutputBlock {
    layout: ChannelLayout,
    capacity: usize,
    data: Vec<f32>,
    published: Option<Published>,
}

impl OutputBlock {
    /// Allocates `layout.channels() * capacity_frames` zeroed samples once.
    pub fn with_capacity(layout: ChannelLayout, capacity_frames: usize) -> Result<Self, ScoreError> {
        if capacity_frames == 0 {
            return Err(ScoreError::OutOfRange);
        }
        if capacity_frames > MAX_BLOCK_FRAMES {
            return Err(ScoreError::CapacityExceeded);
        }
        let total = layout
            .channels()
            .checked_mul(capacity_frames)
            .ok_or(ScoreError::CapacityExceeded)?;
        Ok(Self {
            layout,
            capacity: capacity_frames,
            data: vec![0.0; total],
            published: None,
        })
    }
    pub fn layout(&self) -> &ChannelLayout {
        &self.layout
    }
    pub fn capacity(&self) -> usize {
        self.capacity
    }
    /// Frames of the last published block; 0 when nothing is published.
    pub fn published_frames(&self) -> usize {
        self.published.map_or(0, |p| p.frames)
    }
    pub fn published_start_sample(&self) -> Option<u64> {
        self.published.map(|p| p.start_sample)
    }
    pub fn published_sample_rate_hz(&self) -> Option<u32> {
        self.published.map(|p| p.sample_rate_hz)
    }
    /// Published samples of one channel (`None` when nothing is published or `ch` is out of range).
    pub fn published_channel(&self, ch: usize) -> Option<&[f32]> {
        let p = self.published?;
        self.data
            .chunks(self.capacity)
            .nth(ch)
            .map(|c| &c[..p.frames])
    }
    /// The whole backing store (all channels, full capacity), published or not.
    pub fn backing(&self) -> &[f32] {
        &self.data
    }
    /// Full-capacity writable channel slices, in layout order. For processors only.
    pub fn channels_mut(&mut self) -> std::slice::ChunksMut<'_, f32> {
        self.data.chunks_mut(self.capacity)
    }
    pub fn channel_mut(&mut self, ch: usize) -> Option<&mut [f32]> {
        self.data.chunks_mut(self.capacity).nth(ch)
    }
    /// First two channels, writable at once.
    pub fn pair_mut(&mut self) -> Option<(&mut [f32], &mut [f32])> {
        if self.layout.channels() < 2 {
            return None;
        }
        let (a, rest) = self.data.split_at_mut(self.capacity);
        Some((a, &mut rest[..self.capacity]))
    }
    pub(crate) fn unpublish(&mut self) {
        self.published = None;
    }
    pub(crate) fn publish(&mut self, frames: usize, start_sample: u64, sample_rate_hz: u32) {
        self.published = Some(Published {
            frames,
            start_sample,
            sample_rate_hz,
        });
    }
}
