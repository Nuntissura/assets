use crate::block::{InputBlock, OutputBlock};
use crate::dsp::Unity;
use crate::engine::Processor;
use crate::error::ScoreError;
use crate::layout::{BlockSpec, ChannelLayout, MAX_BLOCK_FRAMES, MAX_CHANNELS};
use hsk_studio_accord::CancellationToken;

/// Widest effect stack accepted (authored bound).
pub const MAX_CHAIN_STAGES: usize = 64;

/// One ordered stack entry: a processor and the layout it produces.
pub struct ChainStage {
    pub processor: Box<dyn Processor + Send>,
    pub output: ChannelLayout,
}

struct Stage {
    processor: Box<dyn Processor + Send>,
    input: ChannelLayout,
    output: ChannelLayout,
    bypassed: bool,
}

/// Ordered, individually bypassable effect stack (STU-FX-135) that is itself a [`Processor`].
///
/// All intermediate buffers are allocated in [`Chain::new`]; `process` allocates nothing.
/// Latency is the sum over active stages. Bypass is allowed only for layout-preserving stages and
/// removes the stage's latency from the stack (the receipt reports the live total); un-bypassing
/// resets the stage because its history is stale.
pub struct Chain {
    stages: Vec<Stage>,
    specs: Vec<BlockSpec>,
    scratch: Vec<OutputBlock>,
    capacity: usize,
    rate: Option<u32>,
    input: ChannelLayout,
    output: ChannelLayout,
}

impl Chain {
    pub fn new(
        input: ChannelLayout,
        capacity_frames: usize,
        stages: Vec<ChainStage>,
    ) -> Result<Self, ScoreError> {
        if stages.is_empty() || stages.len() > MAX_CHAIN_STAGES {
            return Err(ScoreError::OutOfRange);
        }
        if capacity_frames == 0 || capacity_frames > MAX_BLOCK_FRAMES {
            return Err(ScoreError::OutOfRange);
        }
        let mut rate: Option<u32> = None;
        let mut previous = input.clone();
        let mut built = Vec::with_capacity(stages.len());
        let mut specs = Vec::with_capacity(stages.len());
        let mut scratch = Vec::with_capacity(stages.len().saturating_sub(1));
        let last = stages.len() - 1;
        for (i, stage) in stages.into_iter().enumerate() {
            if !stage.processor.accepts(&previous, &stage.output) {
                return Err(ScoreError::ChannelMismatch);
            }
            match (rate, stage.processor.required_sample_rate_hz()) {
                (Some(a), Some(b)) if a != b => return Err(ScoreError::InvalidRate),
                (None, Some(b)) => rate = Some(b),
                _ => {}
            }
            specs.push(BlockSpec {
                sample_rate_hz: 1,
                layout: previous.clone(),
                frames: 1,
                start_sample: 0,
            });
            if i != last {
                scratch.push(OutputBlock::with_capacity(
                    stage.output.clone(),
                    capacity_frames,
                )?);
            }
            built.push(Stage {
                processor: stage.processor,
                input: previous,
                output: stage.output.clone(),
                bypassed: false,
            });
            previous = stage.output;
        }
        Ok(Self {
            stages: built,
            specs,
            scratch,
            capacity: capacity_frames,
            rate,
            input,
            output: previous,
        })
    }
    pub fn stages(&self) -> usize {
        self.stages.len()
    }
    pub fn output_layout(&self) -> &ChannelLayout {
        &self.output
    }
    pub fn is_bypassed(&self, index: usize) -> bool {
        self.stages.get(index).is_some_and(|s| s.bypassed)
    }
    /// Bypass or re-enable one stage. Only layout-preserving stages can be bypassed.
    pub fn set_bypass(&mut self, index: usize, bypassed: bool) -> Result<(), ScoreError> {
        let stage = self.stages.get_mut(index).ok_or(ScoreError::OutOfRange)?;
        if bypassed && stage.input != stage.output {
            return Err(ScoreError::ChannelMismatch);
        }
        if stage.bypassed && !bypassed {
            stage.processor.reset();
        }
        stage.bypassed = bypassed;
        Ok(())
    }
}

impl Processor for Chain {
    fn latency_samples(&self) -> u32 {
        self.stages
            .iter()
            .filter(|s| !s.bypassed)
            .map(|s| s.processor.latency_samples())
            .sum()
    }
    fn accepts(&self, input: &ChannelLayout, output: &ChannelLayout) -> bool {
        *input == self.input && *output == self.output
    }
    fn required_sample_rate_hz(&self) -> Option<u32> {
        self.rate
    }
    fn max_frames(&self) -> Option<usize> {
        Some(self.capacity)
    }
    fn process(
        &mut self,
        input: &InputBlock<'_>,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
    ) -> Result<(), ScoreError> {
        let frames = input.frames();
        if frames > self.capacity {
            return Err(ScoreError::CapacityExceeded);
        }
        let Some(last) = self.stages.iter().rposition(|s| !s.bypassed) else {
            return Unity.process(input, out, cancel);
        };
        let (rate, start) = (input.sample_rate_hz(), input.start_sample());
        let boundary = self.scratch.len();
        let mut previous: Option<usize> = None;
        for i in 0..=last {
            if self.stages[i].bypassed {
                continue;
            }
            let spec = &mut self.specs[i];
            (spec.sample_rate_hz, spec.frames, spec.start_sample) = (rate, frames, start);
            let (done, rest) = self.scratch.split_at_mut(i.min(boundary));
            let mut slices: [&[f32]; MAX_CHANNELS] = [&[]; MAX_CHANNELS];
            let stage_input = match previous {
                None => *input,
                Some(p) => {
                    let block = &done[p];
                    for (slot, channel) in slices
                        .iter_mut()
                        .zip(block.backing().chunks(block.capacity()))
                    {
                        *slot = &channel[..frames];
                    }
                    InputBlock::new(&self.specs[i], &slices[..self.stages[i].input.channels()])
                }
            };
            let target = if i == last { &mut *out } else { &mut rest[0] };
            self.stages[i].processor.process(&stage_input, target, cancel)?;
            if i != last {
                previous = Some(i);
            }
        }
        Ok(())
    }
    fn reset(&mut self) {
        for stage in &mut self.stages {
            stage.processor.reset();
        }
    }
}
