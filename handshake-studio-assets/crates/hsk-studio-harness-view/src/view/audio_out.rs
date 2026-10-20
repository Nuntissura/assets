//! Program monitor audio out (lane SMB-PULSE-PLAYBACK-2, feature `audio-device`): pulse's
//! [`TimelineMixStream`] (any number of clips) feeds a score-device [`OutputStream`] and the
//! device's consumed-frame counter is the playback master clock ([`AudioOut`] implements pulse's
//! `AudioClock`). Off the realtime path: [`AudioOut::pump`] renders whole device blocks into the
//! ring from the host loop; score-device's callback only copies (STU-ARC-008: realtime audio
//! callbacks MUST NOT allocate, block on locks, decode or call the general recorder).
//!
//! Clock: `sample_position = max(consumed, audible_from) - offset`. When audio (re)starts at a
//! position, the samples already queued in the ring still play first: `audible_from` is the
//! device frame at which the new audio becomes audible and `offset` keeps the reading continuous,
//! so the clock stands still until the first new sample is played and the video anchored at that
//! reading starts with it (test `audio_out_plays_the_timeline_mix_through_the_device_in_sync`).
//!
//! Policy: audio plays only at 1x forward; paused, shuttle at another speed or reverse, and past
//! the end of the active range the device gets silence (Premiere's scrub and shuttle audio are
//! UNVERIFIED, D5). Looping wraps the mix at the active range's sample bounds. Devices: tests use
//! score-device's software clocked device exactly as score-device's own tests do (no OS audio);
//! a host opens the real device through score-device (`cpal-backend`) with an explicit grant.
#![deny(clippy::arithmetic_side_effects)]

use super::monitor::{MonitorFail, ProgramMonitor, Transport};
use hsk_studio_accord::CancellationToken;
use hsk_studio_pulse::{AudioClock, AudioSources, PlaybackScheduler, TimelineBusStream, TimelineMixStream, samples_floor};
use hsk_studio_score_device::{CloseReport, OutputStream, PushError};
use serde_json::{Value, json};

fn fail(code: &'static str, detail: impl Into<String>) -> MonitorFail {
    MonitorFail {
        code,
        detail: detail.into(),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioOutStats {
    pub mix_blocks: u64,
    pub silent_blocks: u64,
    pub restarts: u64,
    pub loop_wraps: u64,
}

/// What the output plays (lane SMB-AUDITION-SESSION-2): the plain timeline mix, or the timeline
/// through its submix buses, sends and master (pulse [`TimelineBusStream`] over score's mix graph).
pub enum PlaybackMix<S: AudioSources> {
    Timeline(TimelineMixStream<S>),
    Buses(Box<TimelineBusStream>),
}

/// Source type of a bus-only output ([`AudioOut::with_buses`]); it holds no audio.
pub struct NoTimeline;
impl AudioSources for NoTimeline {
    fn audio(&self, _: &hsk_studio_folio::ClipSource) -> Option<&[Vec<f32>]> {
        None
    }
}

impl<S: AudioSources> PlaybackMix<S> {
    fn rate(&self) -> u32 {
        match self {
            Self::Timeline(m) => m.sample_rate_hz(),
            Self::Buses(b) => b.sample_rate_hz(),
        }
    }
    fn max_block_frames(&self) -> usize {
        match self {
            Self::Timeline(m) => m.max_block_frames(),
            Self::Buses(b) => b.max_block_frames(),
        }
    }
    /// Next sample the mix renders.
    pub fn position(&self) -> u64 {
        match self {
            Self::Timeline(m) => m.position(),
            Self::Buses(b) => b.position(),
        }
    }
    fn seek(&mut self, at: u64, cancel: &CancellationToken) -> Result<(), MonitorFail> {
        match self {
            Self::Timeline(m) => {
                m.seek(at);
                Ok(())
            }
            Self::Buses(b) => b.seek(at, cancel).map_err(|e| fail("audio_mix", e.code())),
        }
    }
    fn render(&mut self, left: &mut [f32], right: &mut [f32], cancel: &CancellationToken) -> Result<(), MonitorFail> {
        match self {
            Self::Timeline(m) => m.render(left, right, cancel),
            Self::Buses(b) => b.render(left, right, cancel),
        }
        .map_err(|e| fail("audio_mix", e.code()))
    }
    /// The plain timeline mix, when that is what plays.
    pub fn timeline(&self) -> Option<&TimelineMixStream<S>> {
        match self {
            Self::Timeline(m) => Some(m),
            Self::Buses(_) => None,
        }
    }
    /// The bus stream, when the buses play.
    pub fn buses(&self) -> Option<&TimelineBusStream> {
        match self {
            Self::Timeline(_) => None,
            Self::Buses(b) => Some(b.as_ref()),
        }
    }
}

/// Timeline mix -> score-device output, and the master clock.
pub struct AudioOut<S: AudioSources> {
    mix: PlaybackMix<S>,
    stream: OutputStream,
    block: usize,
    left: Vec<f32>,
    right: Vec<f32>,
    interleaved: Vec<f32>,
    /// A rendered block the ring refused (kept, pushed first next time).
    pending: bool,
    audible: bool,
    audible_from: u64,
    /// Sequence sample the audio restarted at.
    started_at: u64,
    offset: u64,
    range: (u64, u64),
    looping: bool,
    stats: AudioOutStats,
}

impl<S: AudioSources> AudioOut<S> {
    /// `audio_config` when the stream's rate differs from the mix, it is not stereo, or its
    /// block exceeds the mix's block.
    pub fn new(mix: TimelineMixStream<S>, stream: OutputStream) -> Result<Self, MonitorFail> {
        Self::with_mix(PlaybackMix::Timeline(mix), stream)
    }

    /// As [`AudioOut::new`] for either playback mix.
    pub fn with_mix(mix: PlaybackMix<S>, stream: OutputStream) -> Result<Self, MonitorFail> {
        let config = stream.config().clone();
        let block = usize::try_from(config.block_frames).map_err(|_| fail("audio_config", "block"))?;
        if config.sample_rate_hz != mix.rate()
            || config.layout.channels() != 2
            || block > mix.max_block_frames()
        {
            return Err(fail(
                "audio_config",
                format!(
                    "device {} Hz x {} ch block {} vs mix {} Hz block {}",
                    config.sample_rate_hz,
                    config.layout.channels(),
                    block,
                    mix.rate(),
                    mix.max_block_frames()
                ),
            ));
        }
        let samples = block.checked_mul(2).ok_or_else(|| fail("audio_config", "block"))?;
        Ok(Self {
            mix,
            stream,
            block,
            left: vec![0.0; block],
            right: vec![0.0; block],
            interleaved: vec![0.0; samples],
            pending: false,
            audible: false,
            audible_from: 0,
            started_at: 0,
            offset: 0,
            range: (0, u64::MAX),
            looping: false,
            stats: AudioOutStats::default(),
        })
    }
    pub fn stats(&self) -> AudioOutStats {
        self.stats
    }
    pub fn audible(&self) -> bool {
        self.audible
    }
    pub fn mix(&self) -> &PlaybackMix<S> {
        &self.mix
    }
    pub fn stream(&self) -> &OutputStream {
        &self.stream
    }

    /// Runs a transport command on the monitor with this output as its clock, then follows the
    /// resulting transport (start, restart or silence the audio).
    pub fn command<R>(
        &mut self,
        monitor: &mut ProgramMonitor,
        f: impl FnOnce(&mut ProgramMonitor, &dyn AudioClock) -> R,
    ) -> Result<R, MonitorFail> {
        let r = f(monitor, &*self);
        self.follow(monitor.scheduler())?;
        Ok(r)
    }

    /// Matches the audio to the scheduler: 1x forward playing -> audio from the scheduler's
    /// current position (the clock freezes until it is audible); otherwise silence.
    pub fn follow(&mut self, scheduler: &PlaybackScheduler) -> Result<(), MonitorFail> {
        let rate = self.mix.rate();
        let active = scheduler.active_range();
        let to_samples = |t| samples_floor(t, rate).map_err(|e| fail("audio_position", e.code()));
        self.range = (to_samples(active.start())?, to_samples(active.end())?);
        self.looping = scheduler.looping();
        if scheduler.transport() != Transport::Playing || !scheduler.speed().is_unit_forward() {
            self.audible = false;
            return Ok(());
        }
        let at = to_samples(scheduler.master(&*self).map_err(|e| fail("audio_position", e.code()))?)?;
        let now = self.sample_position();
        let consumed = self.stream.counters().consumed_frames();
        let queued = self.stream.producer_occupied();
        self.audible_from = consumed.saturating_add(queued);
        self.offset = self.audible_from.saturating_sub(now);
        self.mix.seek(at, &CancellationToken::default())?;
        self.started_at = at;
        self.audible = true;
        self.stats.restarts = self.stats.restarts.saturating_add(1);
        Ok(())
    }

    fn render_block(&mut self, cancel: &CancellationToken) -> Result<bool, MonitorFail> {
        let mut filled = 0usize;
        let mut mixed = false;
        while self.audible && filled < self.block {
            let pos = self.mix.position();
            if pos >= self.range.1 {
                if self.looping && self.range.1 > self.range.0 {
                    self.mix.seek(self.range.0, cancel)?;
                    self.stats.loop_wraps = self.stats.loop_wraps.saturating_add(1);
                    continue;
                }
                self.audible = false;
                break;
            }
            let left_in_range = usize::try_from(self.range.1.saturating_sub(pos)).unwrap_or(usize::MAX);
            let n = self.block.saturating_sub(filled).min(left_in_range);
            let end = filled.saturating_add(n);
            self.mix.render(&mut self.left[filled..end], &mut self.right[filled..end], cancel)?;
            filled = end;
            mixed = true;
        }
        self.left[filled..].fill(0.0);
        self.right[filled..].fill(0.0);
        for (i, frame) in self.interleaved.chunks_exact_mut(2).enumerate() {
            frame[0] = self.left[i];
            frame[1] = self.right[i];
        }
        Ok(mixed)
    }

    /// Fills the device ring from the host loop: while audible every free block gets mix audio;
    /// otherwise silence keeps at most two blocks queued (low restart latency). Returns blocks
    /// pushed; `device_lost` when the device went away.
    pub fn pump(&mut self, cancel: &CancellationToken) -> Result<u64, MonitorFail> {
        let mut pushed = 0u64;
        loop {
            let queued = self.stream.producer_occupied();
            let block = u64::try_from(self.block).unwrap_or(u64::MAX);
            if !self.audible && !self.pending && queued >= block.saturating_mul(2) {
                break;
            }
            if !self.pending {
                let mixed = self.render_block(cancel)?;
                if mixed {
                    self.stats.mix_blocks = self.stats.mix_blocks.saturating_add(1);
                } else {
                    self.stats.silent_blocks = self.stats.silent_blocks.saturating_add(1);
                }
                self.pending = true;
            }
            match self.stream.producer().try_push_block(&self.interleaved) {
                Ok(()) => {
                    self.pending = false;
                    pushed = pushed.saturating_add(1);
                }
                Err(PushError::Full) => break,
                Err(PushError::DeviceLost) => return Err(fail("device_lost", "audio device lost")),
                Err(e) => return Err(fail("audio_push", format!("{e:?}"))),
            }
        }
        Ok(pushed)
    }

    /// Sequence sample the device is playing now (loop-aware), `None` while silent or before the
    /// restarted audio is audible.
    pub fn played_sequence_sample(&self) -> Option<u64> {
        let consumed = self.stream.counters().consumed_frames();
        if !self.audible || consumed < self.audible_from {
            return None;
        }
        let n = consumed.saturating_sub(self.audible_from);
        let (start, end) = self.range;
        if self.looping && end > start {
            let span = end.saturating_sub(start);
            let into = self.started_at.saturating_sub(start).saturating_add(n);
            Some(start.saturating_add(into.checked_rem(span).unwrap_or(0)))
        } else {
            Some(self.started_at.saturating_add(n))
        }
    }

    /// Structured projection for models and tests.
    pub fn state_json(&self) -> Value {
        let c = self.stream.counters();
        json!({
            "audible": self.audible,
            "mix_position": self.mix.position(),
            "clock": self.sample_position(),
            "consumed_frames": c.consumed_frames(),
            "underruns": c.underruns(),
            "device_lost": c.is_device_lost(),
            "stats": {"mix_blocks": self.stats.mix_blocks, "silent_blocks": self.stats.silent_blocks, "restarts": self.stats.restarts, "loop_wraps": self.stats.loop_wraps},
        })
    }

    /// Stops and closes the device stream.
    pub fn close(self) -> CloseReport {
        self.stream.stop();
        self.stream.close()
    }
}

/// Frames queued in an output stream's ring (producer view).
trait Occupied {
    fn producer_occupied(&self) -> u64;
}
impl Occupied for OutputStream {
    fn producer_occupied(&self) -> u64 {
        let submitted = self.counters().submitted_frames();
        submitted.saturating_sub(self.counters().consumed_frames())
    }
}

impl AudioOut<NoTimeline> {
    /// Plays the timeline through its buses (pulse [`TimelineBusStream`]).
    pub fn with_buses(buses: TimelineBusStream, stream: OutputStream) -> Result<Self, MonitorFail> {
        Self::with_mix(PlaybackMix::Buses(Box::new(buses)), stream)
    }
}

impl<S: AudioSources> AudioClock for AudioOut<S> {
    fn sample_rate_hz(&self) -> u32 {
        self.mix.rate()
    }
    fn sample_position(&self) -> u64 {
        self.stream
            .counters()
            .consumed_frames()
            .max(self.audible_from)
            .saturating_sub(self.offset)
    }
}
