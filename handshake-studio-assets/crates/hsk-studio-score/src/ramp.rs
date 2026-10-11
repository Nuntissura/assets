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
/// becomes `target`.
///
/// Discard semantics. The pre-block value is snapshotted as the very first act of `process`, before
/// the cancel poll, and stays a live rollback only while the block it belongs to is unpublished.
/// `reset` (the engine discarded the block) restores that snapshot; once the block is published
/// (`commit`, or the start of the next `process`) the snapshot is dropped and `reset` is a no-op.
/// So a canceled or discarded block leaves `current` exactly as it was before the block, and a
/// stage that never ran in the discarded block (later stages of a canceled `Chain`) keeps the value
/// of the last published block instead of reverting a ramp that was already audible.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RampedGain {
    before: f32,
    current: f32,
    target: f32,
    /// `true` while the last processed block is not yet known to be published.
    uncommitted: bool,
}

impl RampedGain {
    pub fn new(linear: f32) -> Result<Self, ScoreError> {
        if linear.is_finite() {
            Ok(Self {
                before: linear,
                current: linear,
                target: linear,
                uncommitted: false,
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
    fn identity(&self) -> String {
        format!(
            "ramped_gain:current={:08x};target={:08x}",
            self.current.to_bits(),
            self.target.to_bits()
        )
    }
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
        // A new block starts only after the previous one was published (a discard calls `reset`),
        // so that block is committed; snapshot before any early return so a cancel at the poll
        // below or inside `per_channel` restores exactly this value.
        self.uncommitted = false;
        self.before = self.current;
        poll(cancel)?;
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
        self.uncommitted = true;
        Ok(())
    }
    fn reset(&mut self) {
        if self.uncommitted {
            self.current = self.before;
            self.uncommitted = false;
        }
    }
    fn commit(&mut self) {
        self.uncommitted = false;
    }
}
