use crate::block::{InputBlock, OutputBlock};
use crate::error::ScoreError;
use crate::layout::{ChannelLayout, MAX_CHANNELS, sample_to_ticks};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, Ticks};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// One audio transform over a whole admitted block.
///
/// Contract: after admission, write exactly `input.frames()` frames into the first
/// `input.frames()` samples of every output channel; allocate nothing; poll `cancel` at least
/// every `CANCEL_CHECK_FRAMES` frames and return `Canceled`. Output is published only by the
/// engine. `reset` returns all internal history to its initial state (called when a block is
/// discarded, because the history then contains unpublished samples).
pub trait Processor {
    /// Delay, in samples, between an input sample and its place in the output stream.
    fn latency_samples(&self) -> u32;
    /// Whether this processor can map `input` to `output` (no allocation).
    fn accepts(&self, input: &ChannelLayout, output: &ChannelLayout) -> bool;
    /// Rate this processor was built for; the engine rejects other block rates.
    fn required_sample_rate_hz(&self) -> Option<u32> {
        None
    }
    fn process(
        &mut self,
        input: &InputBlock<'_>,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
    ) -> Result<(), ScoreError>;
    fn reset(&mut self) {}
}

/// Explicit request identity. `actor: None` is rejected before any PCM access.
#[derive(Clone, Debug)]
pub struct RequestContext {
    pub resource: DomainId,
    pub actor: Option<ActorContext>,
    pub expected_revision: u64,
}

/// Allocation-free (`Copy`) receipt of one published block.
///
/// `start_sample`/`end_sample` position the block in the OUTPUT stream; the content is delayed by
/// `latency_samples` relative to the input timeline (latency compensation is the caller's).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockReceipt {
    pub processed_frames: usize,
    pub latency_samples: u32,
    pub sample_rate_hz: u32,
    pub start_sample: u64,
    pub end_sample: u64,
    pub revision: u64,
    channels: usize,
    labels: [u16; MAX_CHANNELS],
    peak: [f32; MAX_CHANNELS],
}

impl BlockReceipt {
    /// Output channel labels, in layout order.
    pub fn labels(&self) -> &[u16] {
        &self.labels[..self.channels]
    }
    /// Per-channel absolute peak of the published output.
    pub fn peak(&self) -> &[f32] {
        &self.peak[..self.channels]
    }
    pub fn start_ticks(&self) -> Result<Ticks, ScoreError> {
        sample_to_ticks(self.start_sample, self.sample_rate_hz)
    }
    pub fn end_ticks(&self) -> Result<Ticks, ScoreError> {
        sample_to_ticks(self.end_sample, self.sample_rate_hz)
    }
}

/// RAII claim on the engine's single block slot; released on every exit path.
#[derive(Debug)]
pub struct Reservation<'e> {
    busy: &'e AtomicBool,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.busy.store(false, Ordering::Release);
    }
}

/// One synchronous admitted block at a time per consumer; bound to one resource and revision.
#[derive(Debug)]
pub struct Engine {
    resource: DomainId,
    revision: AtomicU64,
    busy: AtomicBool,
}

impl Engine {
    pub fn new(resource: DomainId, revision: u64) -> Self {
        Self {
            resource,
            revision: AtomicU64::new(revision),
            busy: AtomicBool::new(false),
        }
    }
    pub fn resource(&self) -> &DomainId {
        &self.resource
    }
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
    /// Advances the live revision; a block in flight for an older revision cannot publish.
    pub fn set_revision(&self, revision: u64) {
        self.revision.store(revision, Ordering::Release);
    }
    /// Claims the slot without spinning; a held reservation makes `process` return `Busy`.
    pub fn reserve(&self) -> Result<Reservation<'_>, ScoreError> {
        self.busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| ScoreError::Busy)?;
        Ok(Reservation { busy: &self.busy })
    }

    /// Admits, processes and publishes one block.
    ///
    /// Everything that can be refused is checked before the first write to `out`; a refused
    /// request leaves `out` (including its previous publication) untouched. After the block
    /// begins, it either completes and publishes, or is discarded unpublished (processor reset).
    pub fn process(
        &self,
        ctx: &RequestContext,
        input: &InputBlock<'_>,
        proc: &mut dyn Processor,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
    ) -> Result<BlockReceipt, ScoreError> {
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        if ctx.actor.is_none() {
            return Err(ScoreError::MissingContext);
        }
        if ctx.resource != self.resource {
            return Err(ScoreError::ResourceMismatch);
        }
        if ctx.expected_revision != self.revision() {
            return Err(ScoreError::StaleRevision);
        }
        let _slot = self.reserve()?;

        input.validate()?;
        let spec = input.spec();
        if proc
            .required_sample_rate_hz()
            .is_some_and(|rate| rate != spec.sample_rate_hz)
        {
            return Err(ScoreError::InvalidRate);
        }
        if !proc.accepts(&spec.layout, out.layout()) {
            return Err(ScoreError::ChannelMismatch);
        }
        if spec.frames > out.capacity() {
            return Err(ScoreError::CapacityExceeded);
        }
        let end_sample = spec.end_sample()?;

        out.unpublish();
        let settled = proc
            .process(input, out, cancel)
            .and_then(|()| cancel.check().map_err(|_| ScoreError::Canceled))
            .and_then(|()| {
                if ctx.expected_revision == self.revision() {
                    Ok(())
                } else {
                    Err(ScoreError::StaleRevision)
                }
            });
        if let Err(error) = settled {
            proc.reset();
            return Err(error);
        }

        let channels = out.layout().channels();
        let mut receipt = BlockReceipt {
            processed_frames: spec.frames,
            latency_samples: proc.latency_samples(),
            sample_rate_hz: spec.sample_rate_hz,
            start_sample: spec.start_sample,
            end_sample,
            revision: ctx.expected_revision,
            channels,
            labels: [0; MAX_CHANNELS],
            peak: [0.0; MAX_CHANNELS],
        };
        receipt.labels[..channels].copy_from_slice(out.layout().labels());
        for (slot, samples) in receipt.peak[..channels]
            .iter_mut()
            .zip(out.channels_mut().map(|c| &c[..spec.frames]))
        {
            *slot = samples.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
        }
        out.publish(spec.frames, spec.start_sample, spec.sample_rate_hz);
        Ok(receipt)
    }
}
