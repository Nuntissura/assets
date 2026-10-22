//! Push-style processor stage for streaming bounces: the same block boundaries, latency
//! compensation, publication gates and meters as [`Engine::bounce`](crate::Engine::bounce), but
//! fed in arbitrary pieces and emitting retained frames downstream as they become available.
use crate::block::{InputBlock, OutputBlock};
use crate::bounce::BounceRun;
use crate::engine::{Engine, Processor, RequestContext};
use crate::error::ScoreError;
use crate::layout::{BlockSpec, ChannelLayout, MAX_CHANNELS};
use hsk_studio_accord::CancellationToken;

/// Receiver of retained frames: `(offset in the retained stream, one slice per output channel)`.
pub(crate) type Downstream<'a> = &'a mut dyn FnMut(u64, &[&[f32]]) -> Result<(), ScoreError>;

/// Where a stage runs and what it runs on.
pub(crate) struct StageSetup<'a> {
    pub(crate) engine: &'a Engine,
    pub(crate) ctx: &'a RequestContext,
    pub(crate) cancel: &'a CancellationToken,
    pub(crate) layout: &'a ChannelLayout,
    pub(crate) sample_rate_hz: u32,
    pub(crate) start_sample: u64,
    pub(crate) block_frames: usize,
    /// Real frames the stage will be fed.
    pub(crate) frames: usize,
}

pub(crate) struct StageRunner<'a> {
    engine: &'a Engine,
    ctx: &'a RequestContext,
    cancel: &'a CancellationToken,
    proc: &'a mut dyn Processor,
    out: &'a mut OutputBlock,
    in_channels: usize,
    out_channels: usize,
    block: usize,
    latency: usize,
    frames: usize,
    fed: usize,
    at: usize,
    staged: usize,
    stage: Vec<f32>,
    spec: BlockSpec,
    base_start: u64,
    run: BounceRun,
}

impl<'a> StageRunner<'a> {
    /// Resets `proc` (a bounce starts from clean state) and allocates the one block of staging
    /// (`channels * block_frames` samples); nothing else is allocated while running.
    pub(crate) fn new(
        setup: &StageSetup<'a>,
        proc: &'a mut dyn Processor,
        out: &'a mut OutputBlock,
    ) -> Self {
        proc.reset();
        let in_channels = setup.layout.channels();
        let out_channels = out.layout().channels();
        Self {
            engine: setup.engine,
            ctx: setup.ctx,
            cancel: setup.cancel,
            latency: proc.latency_samples() as usize,
            proc,
            out,
            in_channels,
            out_channels,
            block: setup.block_frames,
            frames: setup.frames,
            fed: 0,
            at: 0,
            staged: 0,
            stage: vec![0.0; in_channels * setup.block_frames],
            spec: BlockSpec {
                sample_rate_hz: setup.sample_rate_hz,
                layout: setup.layout.clone(),
                frames: setup.block_frames,
                start_sample: setup.start_sample,
            },
            base_start: setup.start_sample,
            run: BounceRun::new(out_channels),
        }
    }

    /// Bytes of staging this stage holds.
    pub(crate) fn staging_bytes(&self) -> usize {
        self.stage.len() * std::mem::size_of::<f32>()
    }

    pub(crate) fn push(
        &mut self,
        input: &[&[f32]],
        down: Downstream<'_>,
    ) -> Result<(), ScoreError> {
        if input.len() != self.in_channels {
            return Err(ScoreError::ChannelMismatch);
        }
        let frames = input.first().map_or(0, |c| c.len());
        if input.iter().any(|c| c.len() != frames) || frames > self.frames - self.fed {
            return Err(ScoreError::LengthMismatch);
        }
        self.feed(frames, Some(input), down)?;
        self.fed += frames;
        Ok(())
    }

    /// Renders the latency tail and the last partial block; returns the meters and digests of
    /// everything retained. All real frames must have been pushed.
    pub(crate) fn finish(&mut self, down: Downstream<'_>) -> Result<BounceRun, ScoreError> {
        if self.fed != self.frames {
            return Err(ScoreError::LengthMismatch);
        }
        self.feed(self.latency, None, down)?;
        if self.staged > 0 {
            let len = std::mem::take(&mut self.staged);
            self.run_block(len, down)?;
        }
        Ok(std::mem::replace(&mut self.run, BounceRun::new(0)))
    }

    fn feed(
        &mut self,
        frames: usize,
        input: Option<&[&[f32]]>,
        down: Downstream<'_>,
    ) -> Result<(), ScoreError> {
        let block = self.block;
        let mut done = 0;
        while done < frames {
            let at = self.staged;
            let take = (block - at).min(frames - done);
            for ch in 0..self.in_channels {
                let dst = &mut self.stage[ch * block + at..ch * block + at + take];
                match input {
                    Some(source) => dst.copy_from_slice(&source[ch][done..done + take]),
                    None => dst.fill(0.0),
                }
            }
            self.staged += take;
            done += take;
            if self.staged == block {
                self.staged = 0;
                self.run_block(block, down)?;
            }
        }
        Ok(())
    }

    fn run_block(&mut self, len: usize, down: Downstream<'_>) -> Result<(), ScoreError> {
        self.engine.settle(self.ctx, self.cancel)?;
        self.spec.frames = len;
        self.spec.start_sample = self.base_start + self.at as u64;
        let mut views: [&[f32]; MAX_CHANNELS] = [&[]; MAX_CHANNELS];
        for (slot, chunk) in views.iter_mut().zip(self.stage.chunks(self.block)) {
            *slot = &chunk[..len];
        }
        let input = InputBlock::new(&self.spec, &views[..self.in_channels]);
        self.engine.run_block(
            self.ctx,
            &input,
            &mut *self.proc,
            &mut *self.out,
            self.cancel,
        )?;
        self.run.blocks += 1;

        let at = self.at;
        let (lo, hi) = (
            at.max(self.latency),
            (at + len).min(self.latency + self.frames),
        );
        if lo < hi {
            let mut kept: [&[f32]; MAX_CHANNELS] = [&[]; MAX_CHANNELS];
            for (ch, slot) in kept.iter_mut().enumerate().take(self.out_channels) {
                let samples = self
                    .out
                    .published_channel(ch)
                    .ok_or(ScoreError::ChannelMismatch)?;
                *slot = &samples[lo - at..hi - at];
            }
            self.run.record(&kept[..self.out_channels]);
            down((lo - self.latency) as u64, &kept[..self.out_channels])?;
        }
        self.at += len;
        Ok(())
    }
}
