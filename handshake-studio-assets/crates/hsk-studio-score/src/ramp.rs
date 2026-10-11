use crate::block::{InputBlock, OutputBlock};
use crate::dsp::{per_channel, poll};
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::ChannelLayout;
use hsk_studio_accord::CancellationToken;

/// Gain that moves linearly from its current value to a target across each block, so a changed
/// `volume` (STU-FX-137) never steps. Frame `n` of an `N`-frame block uses
/// `g(n) = current + (target - current) * n / N` (computed in f64); the block ends one step short
/// of the target, so the next block starts exactly on it. After a successful block `current`
/// becomes `target`. When the engine discards a block, `reset` restores the pre-block value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RampedGain {
    before: f32,
    current: f32,
    target: f32,
}

impl RampedGain {
    pub fn new(linear: f32) -> Result<Self, ScoreError> {
        if linear.is_finite() {
            Ok(Self {
                before: linear,
                current: linear,
                target: linear,
            })
        } else {
            Err(ScoreError::NotFinite)
        }
    }
    pub fn set_target(&mut self, target: f32) -> Result<(), ScoreError> {
        if target.is_finite() {
            self.target = target;
            Ok(())
        } else {
            Err(ScoreError::NotFinite)
        }
    }
    pub fn current(&self) -> f32 {
        self.current
    }
    pub fn target(&self) -> f32 {
        self.target
    }
}

impl Processor for RampedGain {
    fn latency_samples(&self) -> u32 {
        0
    }
    fn accepts(&self, input: &ChannelLayout, output: &ChannelLayout) -> bool {
        input == output
    }
    fn process(
        &mut self,
        input: &InputBlock<'_>,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
    ) -> Result<(), ScoreError> {
        poll(cancel)?;
        self.before = self.current;
        let (start, delta) = (
            f64::from(self.current),
            f64::from(self.target) - f64::from(self.current),
        );
        let frames = input.frames() as f64;
        let mut at = 0_usize;
        let mut last_channel = usize::MAX;
        per_channel(input, out, cancel, |ch, src, dst| {
            if ch != last_channel {
                (last_channel, at) = (ch, 0);
            }
            for (k, (d, s)) in dst.iter_mut().zip(src).enumerate() {
                let gain = start + delta * ((at + k) as f64 / frames);
                *d = (f64::from(*s) * gain) as f32;
            }
            at += src.len();
        })?;
        self.current = self.target;
        Ok(())
    }
    fn reset(&mut self) {
        self.current = self.before;
    }
}
