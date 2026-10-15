//! Decoded-frame cache, decoder port and prefetch for the program monitor (PR-19 playback with
//! real media; STU-ARC-008 bounded admission: decoded frames and bytes in flight are admitted
//! before any allocation, overload is a typed refusal, every counter is observable).
//!
//! - [`FrameDecoder`] is the media-owner port: `declared_bytes` answers from container metadata
//!   alone (no pixel allocation) how many bytes one decoded frame of a source will hold, then
//!   `decode_run` decodes up to `max_frames` frames starting at the frame holding a source tick
//!   and returns each with the source span it covers. A refusal is a [`DecodeRefusal`] with a
//!   stable code (for example `codec_unavailable`), never an empty frame.
//! - [`FrameCache`] keeps decoded frames keyed by source and source span, least recently used out
//!   first, bounded by [`FrameCacheLimits`] (total bytes, entries, bytes per frame). A frame whose
//!   declared size is over the per-frame or total budget is refused before the decoder runs
//!   (`limit_exceeded`; test `frame_pipeline_caches_prefetches_and_refuses_before_allocation`).
//! - [`FramePipeline`] puts the two together: cache hit, else admit + decode a run + insert; a
//!   source that refused is remembered (bounded) so a missing codec is reported once per source
//!   and not re-decoded every frame; [`FramePipeline::prefetch`] decodes the frames the scheduler
//!   will present next ([`PlaybackScheduler::upcoming_frames`] + [`prefetch_layers`]) within a
//!   per-call decode budget.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::{PulseError, Result};
use crate::playback::{FrameLayer, PlaybackScheduler, frame_layers};
use crate::range::TickRange;
use crate::timeline_edit::SourceExtents;
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{ClipSource, StudioSequence};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Longest source key (bytes) the cache accepts.
pub const MAX_SOURCE_KEY_BYTES: usize = 1024;
/// Most frames one `upcoming_frames` / prefetch plan may name.
pub const MAX_PREFETCH_FRAMES: usize = 256;
/// Most sources remembered as refused.
pub const MAX_REFUSED_SOURCES: usize = 256;

/// Bytes a cached frame holds.
pub trait FrameCost {
    fn cost_bytes(&self) -> u64;
}

/// Stable refusal of a decoder (code from the media owner, detail for humans).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeRefusal {
    pub code: &'static str,
    pub detail: String,
}

impl DecodeRefusal {
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

/// One decoded frame and the source span `[start, end)` it presents.
pub struct DecodedFrame<T> {
    pub span: TickRange,
    pub frame: T,
}

/// Typed port: the media owner's decoder.
pub trait FrameDecoder {
    type Frame: FrameCost;
    /// Bytes one decoded frame of `source` will hold, from metadata only (no pixel allocation).
    fn declared_bytes(
        &self,
        source: &ClipSource,
        cancel: &CancellationToken,
    ) -> std::result::Result<u64, DecodeRefusal>;
    /// Decodes up to `max_frames` (>= 1) frames from the frame holding `source_tick`, in
    /// presentation order.
    fn decode_run(
        &self,
        source: &ClipSource,
        source_tick: Ticks,
        max_frames: u32,
        cancel: &CancellationToken,
    ) -> std::result::Result<Vec<DecodedFrame<Self::Frame>>, DecodeRefusal>;
}

/// Cache bounds (authored values, not spec values).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameCacheLimits {
    pub max_bytes: u64,
    pub max_entries: usize,
    pub max_frame_bytes: u64,
}

impl Default for FrameCacheLimits {
    fn default() -> Self {
        Self {
            max_bytes: 512 * 1024 * 1024,
            max_entries: 512,
            max_frame_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub inserts: u64,
    pub evictions: u64,
    /// Admissions refused (declared or actual size over budget).
    pub refused: u64,
    /// Frames decoded by prefetch.
    pub prefetched: u64,
    /// Decoder calls.
    pub decodes: u64,
}

/// Canonical cache key of a source; `None` when it is longer than [`MAX_SOURCE_KEY_BYTES`].
pub fn source_key(source: &ClipSource) -> Option<String> {
    let key = match source {
        ClipSource::Media {
            artifact_manifest_id,
            content_digest,
        } => format!(
            "media\u{1f}{artifact_manifest_id}\u{1f}{}\u{1f}{}",
            content_digest.algorithm, content_digest.digest
        ),
        ClipSource::Sequence { sequence_id } => format!("sequence\u{1f}{sequence_id}"),
        ClipSource::Primitive {
            schema_id,
            object_id,
        } => format!("primitive\u{1f}{schema_id}\u{1f}{object_id}"),
    };
    (key.len() <= MAX_SOURCE_KEY_BYTES).then_some(key)
}

struct Slot<T> {
    end: u64,
    value: Arc<T>,
    cost: u64,
    last_use: u64,
}

/// LRU cache of decoded frames keyed by (source, span start).
pub struct FrameCache<T> {
    limits: FrameCacheLimits,
    used: u64,
    clock: u64,
    slots: BTreeMap<(String, u64), Slot<T>>,
    stats: CacheStats,
}

impl<T: FrameCost> FrameCache<T> {
    /// `invalid_input` for a zero bound or a per-frame bound above the total.
    pub fn new(limits: FrameCacheLimits) -> Result<Self> {
        if limits.max_bytes == 0
            || limits.max_entries == 0
            || limits.max_frame_bytes == 0
            || limits.max_frame_bytes > limits.max_bytes
        {
            return Err(PulseError::InvalidInput);
        }
        Ok(Self {
            limits,
            used: 0,
            clock: 0,
            slots: BTreeMap::new(),
            stats: CacheStats::default(),
        })
    }
    pub fn limits(&self) -> FrameCacheLimits {
        self.limits
    }
    pub fn stats(&self) -> CacheStats {
        self.stats
    }
    pub fn used_bytes(&self) -> u64 {
        self.used
    }
    pub fn len(&self) -> usize {
        self.slots.len()
    }
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    fn tick(&mut self) -> u64 {
        self.clock = self.clock.saturating_add(1);
        self.clock
    }

    fn find(&self, key: &str, tick: Ticks) -> Option<(String, u64)> {
        let (k, slot) = self
            .slots
            .range(..=(key.to_owned(), tick.value()))
            .next_back()?;
        (k.0 == key && slot.end > tick.value()).then(|| k.clone())
    }

    /// The cached frame of `source` whose span holds `tick` (counts a hit or a miss).
    pub fn get(&mut self, source: &ClipSource, tick: Ticks) -> Option<Arc<T>> {
        let found = source_key(source).and_then(|key| self.find(&key, tick));
        match found {
            Some(k) => {
                let now = self.tick();
                let slot = self.slots.get_mut(&k)?;
                slot.last_use = now;
                self.stats.hits = self.stats.hits.saturating_add(1);
                Some(slot.value.clone())
            }
            None => {
                self.stats.misses = self.stats.misses.saturating_add(1);
                None
            }
        }
    }

    /// True when a frame of `source` holding `tick` is cached (no statistics, no LRU touch).
    pub fn contains(&self, source: &ClipSource, tick: Ticks) -> bool {
        source_key(source).is_some_and(|key| self.find(&key, tick).is_some())
    }

    /// Admission of a frame of `bytes` before anything is decoded or allocated:
    /// `limit_exceeded` above the per-frame or total budget.
    pub fn admit(&mut self, bytes: u64) -> Result<()> {
        if bytes > self.limits.max_frame_bytes || bytes > self.limits.max_bytes {
            self.stats.refused = self.stats.refused.saturating_add(1);
            return Err(PulseError::LimitExceeded);
        }
        Ok(())
    }

    fn evict_one(&mut self) -> bool {
        let Some(k) = self
            .slots
            .iter()
            .min_by_key(|(_, s)| s.last_use)
            .map(|(k, _)| k.clone())
        else {
            return false;
        };
        if let Some(slot) = self.slots.remove(&k) {
            self.used = self.used.saturating_sub(slot.cost);
            self.stats.evictions = self.stats.evictions.saturating_add(1);
        }
        true
    }

    /// Inserts a decoded frame for `span` of `source`, evicting least recently used frames until
    /// it fits. `limit_exceeded` (nothing evicted) when the frame alone is over budget;
    /// `invalid_input` for an empty span or an over-long source key.
    pub fn insert(&mut self, source: &ClipSource, span: TickRange, value: Arc<T>) -> Result<()> {
        if span.is_empty() {
            return Err(PulseError::InvalidInput);
        }
        let key = source_key(source).ok_or(PulseError::InvalidInput)?;
        let cost = value.cost_bytes();
        self.admit(cost)?;
        if let Some(old) = self.slots.remove(&(key.clone(), span.start().value())) {
            self.used = self.used.saturating_sub(old.cost);
        }
        while self.slots.len() >= self.limits.max_entries
            || self.used.saturating_add(cost) > self.limits.max_bytes
        {
            if !self.evict_one() {
                break;
            }
        }
        let now = self.tick();
        self.used = self.used.checked_add(cost).ok_or(PulseError::Overflow)?;
        self.slots.insert(
            (key, span.start().value()),
            Slot {
                end: span.end().value(),
                value,
                cost,
                last_use: now,
            },
        );
        self.stats.inserts = self.stats.inserts.saturating_add(1);
        Ok(())
    }

    /// Drops every frame of `source` (relink or a new revision).
    pub fn invalidate(&mut self, source: &ClipSource) {
        let Some(key) = source_key(source) else {
            return;
        };
        let doomed: Vec<(String, u64)> = self
            .slots
            .range((key.clone(), 0)..)
            .take_while(|(k, _)| k.0 == key)
            .map(|(k, _)| k.clone())
            .collect();
        for k in doomed {
            if let Some(slot) = self.slots.remove(&k) {
                self.used = self.used.saturating_sub(slot.cost);
            }
        }
    }
}

impl PlaybackScheduler {
    /// The next `count` frames this scheduler will present, in presentation order: from the frame
    /// on screen (else the playhead) in the play direction, stepping by the whole part of the
    /// speed (at least 1), wrapping when looping, stopping at the active range edge otherwise.
    /// `limit_exceeded` above [`MAX_PREFETCH_FRAMES`].
    pub fn upcoming_frames(&self, count: usize) -> Result<Vec<u64>> {
        if count > MAX_PREFETCH_FRAMES {
            return Err(PulseError::LimitExceeded);
        }
        let active = self.active_range();
        let first = self.frame_of(active.start());
        let last = self.frame_of(Ticks::new(active.end().value().saturating_sub(1)));
        let n = last.checked_sub(first).and_then(|d| d.checked_add(1)).ok_or(PulseError::Overflow)?;
        let current = self.frame_of(self.playhead()).clamp(first, last);
        let speed = self.speed();
        let stride = u64::from(speed.numerator().unsigned_abs())
            .checked_div(u64::from(speed.denominator()))
            .unwrap_or(1)
            .max(1);
        let mut out = Vec::with_capacity(count);
        let mut at = current.checked_sub(first).ok_or(PulseError::Underflow)?;
        for _ in 0..count {
            let next = if speed.is_reverse() {
                match at.checked_sub(stride) {
                    Some(v) => v,
                    None if self.looping() => {
                        // Wrap: at - stride mod n.
                        let back = stride.checked_rem(n).ok_or(PulseError::Overflow)?;
                        at.checked_add(n).and_then(|v| v.checked_sub(back)).ok_or(PulseError::Overflow)?
                            .checked_rem(n)
                            .ok_or(PulseError::Overflow)?
                    }
                    None => break,
                }
            } else {
                let v = at.checked_add(stride).ok_or(PulseError::Overflow)?;
                if v < n {
                    v
                } else if self.looping() {
                    v.checked_rem(n).ok_or(PulseError::Overflow)?
                } else {
                    break;
                }
            };
            at = next;
            out.push(first.checked_add(at).ok_or(PulseError::Overflow)?);
        }
        Ok(out)
    }
}

/// The layers of `frames` (sequence frame indices) in order: what prefetch should decode.
pub fn prefetch_layers(
    sequence: &StudioSequence,
    scheduler: &PlaybackScheduler,
    frames: &[u64],
    extents: &dyn SourceExtents,
    cancel: &CancellationToken,
) -> Result<Vec<FrameLayer>> {
    if frames.len() > MAX_PREFETCH_FRAMES {
        return Err(PulseError::LimitExceeded);
    }
    let mut out = Vec::new();
    for &f in frames {
        cancel.check()?;
        out.extend(frame_layers(sequence, scheduler.frame_start(f)?, extents, cancel)?);
    }
    Ok(out)
}

/// Cache + decoder + refusal memory.
pub struct FramePipeline<D: FrameDecoder> {
    decoder: D,
    cache: FrameCache<D::Frame>,
    /// Frames decoded per miss (a run serves the following frames too).
    run_frames: u32,
    refused: BTreeMap<String, DecodeRefusal>,
}

impl<D: FrameDecoder> FramePipeline<D> {
    /// `invalid_input` for `run_frames` 0 or above [`MAX_PREFETCH_FRAMES`].
    pub fn new(decoder: D, limits: FrameCacheLimits, run_frames: u32) -> Result<Self> {
        if run_frames == 0 || usize::try_from(run_frames).map_or(true, |r| r > MAX_PREFETCH_FRAMES) {
            return Err(PulseError::InvalidInput);
        }
        Ok(Self {
            decoder,
            cache: FrameCache::new(limits)?,
            run_frames,
            refused: BTreeMap::new(),
        })
    }
    pub fn decoder(&self) -> &D {
        &self.decoder
    }
    pub fn cache(&self) -> &FrameCache<D::Frame> {
        &self.cache
    }
    /// The remembered refusal of `source`, if any.
    pub fn refusal(&self, source: &ClipSource) -> Option<&DecodeRefusal> {
        self.refused.get(&source_key(source)?)
    }
    /// Forgets refusals and cached frames of `source` (after a relink or a decoder change).
    pub fn reset_source(&mut self, source: &ClipSource) {
        if let Some(k) = source_key(source) {
            self.refused.remove(&k);
        }
        self.cache.invalidate(source);
    }

    fn remember(&mut self, key: String, refusal: &DecodeRefusal) {
        if self.refused.len() < MAX_REFUSED_SOURCES {
            self.refused.insert(key, refusal.clone());
        }
    }

    fn fill(
        &mut self,
        source: &ClipSource,
        tick: Ticks,
        frames: u32,
        cancel: &CancellationToken,
    ) -> std::result::Result<u64, DecodeRefusal> {
        let key = source_key(source)
            .ok_or_else(|| DecodeRefusal::new("invalid_input", "source key too long"))?;
        if let Some(r) = self.refused.get(&key) {
            return Err(r.clone());
        }
        let declared = match self.decoder.declared_bytes(source, cancel) {
            Ok(b) => b,
            Err(r) => {
                if r.code != "canceled" {
                    self.remember(key, &r);
                }
                return Err(r);
            }
        };
        if self.cache.admit(declared).is_err() {
            let r = DecodeRefusal::new(
                "limit_exceeded",
                format!("decoded frame of {declared} bytes over the frame cache budget"),
            );
            self.remember(key, &r);
            return Err(r);
        }
        self.cache.stats.decodes = self.cache.stats.decodes.saturating_add(1);
        let run = match self.decoder.decode_run(source, tick, frames, cancel) {
            Ok(run) => run,
            Err(r) => {
                if !matches!(r.code, "canceled" | "out_of_range") {
                    self.remember(key, &r);
                }
                return Err(r);
            }
        };
        let mut inserted = 0u64;
        for d in run {
            if d.frame.cost_bytes() > declared {
                return Err(DecodeRefusal::new(
                    "limit_exceeded",
                    "decoded frame larger than declared",
                ));
            }
            self.cache
                .insert(source, d.span, Arc::new(d.frame))
                .map_err(|e| DecodeRefusal::new(e.code(), "frame cache"))?;
            inserted = inserted.saturating_add(1);
        }
        Ok(inserted)
    }

    /// The decoded frame of `source` holding `source_tick`: cache hit, else a decoded run.
    pub fn frame(
        &mut self,
        source: &ClipSource,
        source_tick: Ticks,
        cancel: &CancellationToken,
    ) -> std::result::Result<Arc<D::Frame>, DecodeRefusal> {
        if let Some(f) = self.cache.get(source, source_tick) {
            return Ok(f);
        }
        self.fill(source, source_tick, self.run_frames, cancel)?;
        self.cache.get(source, source_tick).ok_or_else(|| {
            DecodeRefusal::new(
                "frame_unavailable",
                format!("no decoded frame holds source tick {}", source_tick.value()),
            )
        })
    }

    /// Decodes the frames of `layers` that are not cached, at most `max_decodes` decoder calls.
    /// Returns the frames inserted. Refusals are remembered and skipped, not reported here (the
    /// presenter reports them when the frame is due).
    pub fn prefetch(
        &mut self,
        layers: &[FrameLayer],
        max_decodes: u32,
        cancel: &CancellationToken,
    ) -> u64 {
        let mut calls = 0u32;
        let mut inserted = 0u64;
        for layer in layers {
            if calls >= max_decodes || cancel.check().is_err() {
                break;
            }
            if self.cache.contains(&layer.source, layer.source_tick)
                || self.refusal(&layer.source).is_some()
            {
                continue;
            }
            calls = calls.saturating_add(1);
            if let Ok(n) = self.fill(&layer.source, layer.source_tick, self.run_frames, cancel) {
                inserted = inserted.saturating_add(n);
            }
        }
        self.cache.stats.prefetched = self.cache.stats.prefetched.saturating_add(inserted);
        inserted
    }
}
