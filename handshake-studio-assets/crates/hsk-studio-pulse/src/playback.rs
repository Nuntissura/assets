//! Playback scheduling (PR-19; STU-VID-023 monitor transport: play, stop, step forward, step
//! back, play in-to-out). Pulse decides WHICH frame is due and WHAT plays in it; decoding and
//! pixels belong to the media and composite owners.
//!
//! Clock model: audio is the master. An [`AudioClock`] port reports the audio device's played
//! sample position (score's `ScoreSession::position()` is one implementation, wired by the host;
//! [`HeadlessClock`] is the deterministic one for tests and offline runs). On `play` the scheduler
//! anchors `(master sample, sequence tick)`; afterwards master time in sequence ticks is
//! `anchor_tick + ticks_from_samples(samples - anchor_samples)`, exact for every rate that divides
//! 254016000000 ticks/s (48 kHz: 5292000 ticks per sample).
//!
//! Frame choice at each [`PlaybackScheduler::poll`]: the due frame is
//! `floor(master_ticks / ticks_per_frame)`.
//! - [`DropPolicy::DropLate`] (real-time monitor): present the due frame; frames the master passed
//!   while the presenter was busy are dropped and counted. Invariant (test
//!   `playback_stays_within_one_frame_of_the_audio_clock`): at every presentation
//!   `0 <= master - frame_start < ticks_per_frame`, i.e. video never leads audio and lags it by
//!   less than one frame period (40 ms at 25 fps).
//! - [`DropPolicy::PresentEveryFrame`] (review/export pacing): never skips; when the presenter is
//!   slower than real time the video lags the master and the lag is reported in
//!   [`PlaybackStats::max_lag_ticks`], unbounded by construction.
//!
//! The play range is the work area when set, else the whole sequence (`crate::play_range`).
#![deny(clippy::arithmetic_side_effects)]

use crate::annotate::play_range;
use crate::error::{PulseError, Result};
use crate::range::TickRange;
use crate::time::ticks_from_samples;
use crate::timeline_edit::{SourceExtents, rate_of};
use crate::transition::{clip_source_at, transition_frame};
use hsk_studio_accord::{CancellationToken, FrameRate, Ticks};
use hsk_studio_folio::{ClipSource, StudioSequence, TrackKind};

/// Typed port: the audio master clock (the device's played sample position).
pub trait AudioClock {
    fn sample_rate_hz(&self) -> u32;
    /// Samples played since the stream started (monotonic).
    fn sample_position(&self) -> u64;
}

/// Deterministic headless master clock: the caller advances it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeadlessClock {
    rate_hz: u32,
    samples: u64,
}

impl HeadlessClock {
    pub fn new(rate_hz: u32) -> Self {
        Self {
            rate_hz,
            samples: 0,
        }
    }
    /// Advances by `samples` (saturating at `u64::MAX`).
    pub fn advance(&mut self, samples: u64) {
        self.samples = self.samples.saturating_add(samples);
    }
}

impl AudioClock for HeadlessClock {
    fn sample_rate_hz(&self) -> u32 {
        self.rate_hz
    }
    fn sample_position(&self) -> u64 {
        self.samples
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropPolicy {
    DropLate,
    PresentEveryFrame,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Paused,
    Playing,
    /// Reached the end of the play range.
    Ended,
}

/// What the presenter does now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameAction {
    /// Show frame `frame` (sequence frame index; starts at `tick`).
    Present { frame: u64, tick: Ticks },
    /// The frame on screen is still the due one.
    Hold,
    /// The play range is over; the last frame stays.
    End,
}

/// One scheduler decision with the clock reading it was made at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameDecision {
    pub action: FrameAction,
    /// Master (audio) time in sequence ticks.
    pub master: Ticks,
    /// Frames skipped by this decision (`DropLate`).
    pub dropped: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaybackStats {
    pub presented: u64,
    pub dropped: u64,
    /// Largest `master - frame_start` seen at a presentation, in ticks.
    pub max_lag_ticks: u64,
    /// Times a presented frame started after the master (video ahead of audio); 0 by design.
    pub leads: u64,
}

/// Frame clock and transport over one sequence.
#[derive(Clone, Debug)]
pub struct PlaybackScheduler {
    rate: FrameRate,
    range: TickRange,
    policy: DropPolicy,
    transport: Transport,
    /// Playhead in sequence ticks (frame-aligned while paused).
    playhead: Ticks,
    anchor_samples: u64,
    anchor_tick: Ticks,
    last_frame: Option<u64>,
    stats: PlaybackStats,
}

impl PlaybackScheduler {
    /// Paused at the start of the play range (work area or whole sequence).
    pub fn new(sequence: &StudioSequence, policy: DropPolicy) -> Result<Self> {
        let rate = rate_of(sequence)?;
        let range = play_range(sequence)?;
        if range.is_empty() {
            return Err(PulseError::ZeroDuration);
        }
        Ok(Self {
            rate,
            range,
            policy,
            transport: Transport::Paused,
            playhead: range.start(),
            anchor_samples: 0,
            anchor_tick: range.start(),
            last_frame: None,
            stats: PlaybackStats::default(),
        })
    }
    pub fn rate(&self) -> FrameRate {
        self.rate
    }
    pub fn range(&self) -> TickRange {
        self.range
    }
    pub fn transport(&self) -> Transport {
        self.transport
    }
    pub fn playhead(&self) -> Ticks {
        self.playhead
    }
    pub fn stats(&self) -> PlaybackStats {
        self.stats
    }
    /// Sequence frame index of a tick (floor).
    pub fn frame_of(&self, tick: Ticks) -> u64 {
        tick.value()
            .checked_div(self.rate.ticks_per_frame())
            .unwrap_or(0)
    }
    /// First tick of frame `frame`.
    pub fn frame_start(&self, frame: u64) -> Result<Ticks> {
        crate::time::frames_to_ticks(frame, self.rate)
    }
    fn last_frame_of_range(&self) -> u64 {
        self.frame_of(Ticks::new(self.range.end().value().saturating_sub(1)))
    }

    /// Master time in sequence ticks for the current clock reading.
    pub fn master(&self, clock: &dyn AudioClock) -> Result<Ticks> {
        let played = clock
            .sample_position()
            .checked_sub(self.anchor_samples)
            .ok_or(PulseError::Underflow)?;
        Ok(self
            .anchor_tick
            .checked_add(ticks_from_samples(played, clock.sample_rate_hz())?)?)
    }

    /// Starts (or resumes) playback from the playhead at the clock's current position.
    pub fn play(&mut self, clock: &dyn AudioClock) -> Result<()> {
        ticks_from_samples(1, clock.sample_rate_hz())?;
        if self.transport == Transport::Ended || self.playhead >= self.range.end() {
            self.playhead = self.range.start();
        }
        self.anchor_samples = clock.sample_position();
        self.anchor_tick = self.playhead;
        self.last_frame = None;
        self.transport = Transport::Playing;
        Ok(())
    }

    /// Pauses on the frame on screen (or the due frame when none was presented yet).
    pub fn pause(&mut self, clock: &dyn AudioClock) -> Result<()> {
        if self.transport == Transport::Playing {
            let frame = match self.last_frame {
                Some(f) => f,
                None => self.frame_of(self.master(clock)?.min(self.range.end())),
            };
            self.playhead = self.frame_start(frame.min(self.last_frame_of_range()))?;
        }
        self.transport = Transport::Paused;
        Ok(())
    }

    /// Moves the playhead to the frame holding `tick`, clamped into the play range; while
    /// playing the master is re-anchored there.
    pub fn scrub(&mut self, tick: Ticks, clock: &dyn AudioClock) -> Result<()> {
        let clamped = tick
            .max(self.range.start())
            .min(Ticks::new(self.range.end().value().saturating_sub(1)));
        self.playhead = self
            .frame_start(self.frame_of(clamped))?
            .max(self.range.start());
        if self.transport == Transport::Playing {
            self.anchor_samples = clock.sample_position();
            self.anchor_tick = self.playhead;
            self.last_frame = None;
        } else {
            self.transport = Transport::Paused;
        }
        Ok(())
    }

    /// Step forward (`frames > 0`) or back while paused (pauses first), clamped into the range.
    pub fn step(&mut self, frames: i64, clock: &dyn AudioClock) -> Result<Ticks> {
        self.pause(clock)?;
        let current = self.frame_of(self.playhead);
        let target = if frames < 0 {
            current.saturating_sub(frames.unsigned_abs())
        } else {
            current.saturating_add(frames.unsigned_abs())
        };
        let first = self.frame_of(self.range.start());
        let target = target.clamp(first, self.last_frame_of_range());
        self.playhead = self.frame_start(target)?.max(self.range.start());
        Ok(self.playhead)
    }

    /// The decision for the current clock reading (call once per presenter refresh).
    pub fn poll(&mut self, clock: &dyn AudioClock) -> Result<FrameDecision> {
        if self.transport != Transport::Playing {
            let master = self.playhead;
            let action = if self.transport == Transport::Ended {
                FrameAction::End
            } else {
                let frame = self.frame_of(self.playhead);
                if self.last_frame == Some(frame) {
                    FrameAction::Hold
                } else {
                    self.last_frame = Some(frame);
                    FrameAction::Present {
                        frame,
                        tick: self.playhead,
                    }
                }
            };
            return Ok(FrameDecision {
                action,
                master,
                dropped: 0,
            });
        }
        let master = self.master(clock)?;
        if master >= self.range.end() {
            // Frames the master passed before the end without being shown count as dropped.
            let last_due = self.last_frame_of_range();
            let dropped = match (self.policy, self.last_frame) {
                (DropPolicy::DropLate, Some(last)) => last_due.saturating_sub(last),
                _ => 0,
            };
            self.stats.dropped = self.stats.dropped.saturating_add(dropped);
            self.transport = Transport::Ended;
            self.playhead = self.range.end();
            return Ok(FrameDecision {
                action: FrameAction::End,
                master,
                dropped,
            });
        }
        let due = self.frame_of(master);
        let (frame, dropped) = match (self.policy, self.last_frame) {
            (_, Some(last)) if last >= due => {
                return Ok(FrameDecision {
                    action: FrameAction::Hold,
                    master,
                    dropped: 0,
                });
            }
            (DropPolicy::DropLate, Some(last)) => (due, due.saturating_sub(last).saturating_sub(1)),
            (DropPolicy::PresentEveryFrame, Some(last)) => (last.saturating_add(1), 0),
            (_, None) => (due, 0),
        };
        let tick = self.frame_start(frame)?;
        self.last_frame = Some(frame);
        self.playhead = tick;
        self.stats.presented = self.stats.presented.saturating_add(1);
        self.stats.dropped = self.stats.dropped.saturating_add(dropped);
        match master.checked_sub(tick) {
            Ok(lag) => self.stats.max_lag_ticks = self.stats.max_lag_ticks.max(lag.value()),
            Err(_) => self.stats.leads = self.stats.leads.saturating_add(1),
        }
        Ok(FrameDecision {
            action: FrameAction::Present { frame, tick },
            master,
            dropped,
        })
    }
}

/// One layer of a program-monitor frame, bottom-first: which clip's source plays at which source
/// tick and with what weight (`1` except the incoming side of a cross dissolve).
#[derive(Clone, Debug, PartialEq)]
pub struct FrameLayer {
    pub track_id: String,
    pub clip_id: String,
    pub source: ClipSource,
    pub source_tick: Ticks,
    /// Opacity of this layer over the ones below, `0.0..=1.0`.
    pub weight: f64,
    /// Held end frame (`repeat_end_frame`).
    pub held: bool,
}

/// What plays at `tick`: every enabled video track bottom-to-top (STU-VID-021: the compositing
/// order of video tracks is bottom-to-top), each contributing its enabled clip under `tick`, or
/// during a transition the outgoing clip then the incoming clip at the transition's weight.
pub fn frame_layers(
    sequence: &StudioSequence,
    tick: Ticks,
    extents: &dyn SourceExtents,
    cancel: &CancellationToken,
) -> Result<Vec<FrameLayer>> {
    let mut out = Vec::new();
    for track in sequence
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video && t.enabled)
    {
        cancel.check()?;
        let find = |id: &str| track.clips.iter().find(|c| c.clip_id == id);
        if let Some(tf) = transition_frame(sequence, &track.track_id, tick, extents, cancel)? {
            for (id, source_tick, held, weight) in [
                (
                    &tf.outgoing_clip_id,
                    tf.outgoing_source,
                    tf.outgoing_held,
                    1.0,
                ),
                (
                    &tf.incoming_clip_id,
                    tf.incoming_source,
                    tf.incoming_held,
                    tf.incoming_weight(),
                ),
            ] {
                let clip = find(id).ok_or(PulseError::UnknownClip)?;
                if clip.enabled {
                    out.push(FrameLayer {
                        track_id: track.track_id.clone(),
                        clip_id: clip.clip_id.clone(),
                        source: clip.source_ref.clone(),
                        source_tick,
                        weight,
                        held,
                    });
                }
            }
            continue;
        }
        let under = track.clips.partition_point(|c| c.timeline_start <= tick);
        let Some(clip) = under.checked_sub(1).map(|i| &track.clips[i]) else {
            continue;
        };
        if !clip.enabled || clip.timeline_end().is_none_or(|end| end <= tick) {
            continue;
        }
        let (source_tick, held) = clip_source_at(clip, tick, extents, false)?;
        out.push(FrameLayer {
            track_id: track.track_id.clone(),
            clip_id: clip.clip_id.clone(),
            source: clip.source_ref.clone(),
            source_tick,
            weight: 1.0,
            held,
        });
    }
    Ok(out)
}
