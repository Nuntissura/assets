//! Playback scheduling (PR-19; STU-VID-023 monitor transport: play, stop, step forward, step
//! back, play in-to-out, loop, an in point and an out point; transport command table: shuttle
//! left `J`, shuttle stop `K`, shuttle right `L`, slow shuttle `Shift+J`/`Shift+L`). Pulse decides
//! WHICH frame is due and WHAT plays in it; decoding and pixels belong to the media and composite
//! owners.
//!
//! Clock model: audio is the master. An [`AudioClock`] port reports the audio device's played
//! sample position (score's `ScoreSession::position()` is one implementation, wired by the host;
//! [`HeadlessClock`] is the deterministic one for tests and offline runs). On `play` the scheduler
//! anchors `(master sample, sequence tick)`; afterwards the sequence position is
//! `anchor_tick + speed * ticks_from_samples(samples - anchor_samples)`, exact for every rate that
//! divides 254016000000 ticks/s (48 kHz: 5292000 ticks per sample) and every [`PlaySpeed`]
//! (a reduced ratio with a power-of-two denominator; forward positions round down, reverse
//! positions round up the elapsed distance, so the due frame is always the frame holding the exact
//! position).
//!
//! Active range: the play range (work area when set, else the whole sequence,
//! `crate::play_range`) for `play`, or the in/out span for [`PlaybackScheduler::play_in_to_out`].
//! Looping ([`PlaybackScheduler::set_looping`]) wraps the position into the active range with
//! exact integer arithmetic: unwrapped offset `u` from the range start, cycle
//! `u.div_euclid(len)`, position `start + u.rem_euclid(len)` (test
//! `loop_wraps_exactly_and_shuttles_in_both_directions`). Without looping the range end (forward)
//! or start (reverse) ends playback.
//!
//! Frame choice at each [`PlaybackScheduler::poll`]: the due frame is the frame holding the
//! position. Frames are ordered by a presentation index `p = cycle * n + (frame - first)` (`n`
//! frames per range pass), increasing forward and decreasing in reverse.
//! - [`DropPolicy::DropLate`] (real-time monitor): present the due frame; frames the position
//!   passed while the presenter was busy (and, at 2x/4x/8x shuttle, the frames the speed skips)
//!   are dropped and counted. Invariant (test `playback_stays_within_one_frame_of_the_audio_clock`):
//!   forward, at every presentation `0 <= position - frame_start < ticks_per_frame`; reverse,
//!   `0 < frame_end - position <= ticks_per_frame` (lag in sequence ticks; wall time is that over
//!   the speed).
//! - [`DropPolicy::PresentEveryFrame`] (review/export pacing): never skips, at any speed; when the
//!   presenter is slower than the position the video lags and the lag is reported in
//!   [`PlaybackStats::max_lag_ticks`], unbounded by construction.
//!
//! Shuttle ladder (Studio values; Premiere's exact steps are UNVERIFIED, D5): `L` from stop or
//! reverse plays 1x forward, each further `L` doubles up to [`MAX_SHUTTLE`]; `J` mirrors it in
//! reverse; `K` pauses; `Shift+L`/`Shift+J` play at [`SLOW_SHUTTLE_DENOMINATOR`] (1/2) speed.
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
    /// Reached the end of the play range (its start, in reverse).
    Ended,
}

/// Fastest shuttle multiple.
pub const MAX_SHUTTLE: u32 = 8;
/// Slow shuttle is 1 / this.
pub const SLOW_SHUTTLE_DENOMINATOR: u32 = 2;
/// Largest speed denominator (slowest speed 1/8).
pub const MAX_SPEED_DENOMINATOR: u32 = 8;

/// Signed playback speed `numerator / denominator` (negative = reverse). Admitted: numerator
/// non-zero with `|numerator| / denominator <= MAX_SHUTTLE`, denominator a power of two up to
/// [`MAX_SPEED_DENOMINATOR`], reduced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaySpeed {
    numerator: i32,
    denominator: u32,
}

impl PlaySpeed {
    pub const FORWARD: Self = Self {
        numerator: 1,
        denominator: 1,
    };
    pub const REVERSE: Self = Self {
        numerator: -1,
        denominator: 1,
    };
    /// `zero_speed` for 0; `invalid_speed` for a denominator that is not a power of two up to
    /// [`MAX_SPEED_DENOMINATOR`], an unreduced ratio, or a magnitude above [`MAX_SHUTTLE`].
    pub fn new(numerator: i32, denominator: u32) -> Result<Self> {
        if numerator == 0 {
            return Err(PulseError::ZeroSpeed);
        }
        if !denominator.is_power_of_two() || denominator > MAX_SPEED_DENOMINATOR {
            return Err(PulseError::InvalidSpeed);
        }
        let magnitude = numerator.unsigned_abs();
        if denominator > 1 && magnitude.is_multiple_of(2) {
            return Err(PulseError::InvalidSpeed);
        }
        let max = MAX_SHUTTLE.checked_mul(denominator).ok_or(PulseError::InvalidSpeed)?;
        if magnitude > max {
            return Err(PulseError::InvalidSpeed);
        }
        Ok(Self {
            numerator,
            denominator,
        })
    }
    pub fn numerator(self) -> i32 {
        self.numerator
    }
    pub fn denominator(self) -> u32 {
        self.denominator
    }
    pub fn is_reverse(self) -> bool {
        self.numerator < 0
    }
    /// True for 1x forward (the only speed the audio path plays as is).
    pub fn is_unit_forward(self) -> bool {
        self == Self::FORWARD
    }
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
    /// Sequence position of the master (audio) clock in ticks (wrapped into the range when
    /// looping; clamped to the range start when reverse play ran past it).
    pub master: Ticks,
    /// Frames skipped by this decision (`DropLate`).
    pub dropped: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaybackStats {
    pub presented: u64,
    pub dropped: u64,
    /// Largest lag seen at a presentation, in sequence ticks (see the module docs).
    pub max_lag_ticks: u64,
    /// Times a presented frame was ahead of the master (video ahead of audio); 0 by design.
    pub leads: u64,
    /// Loop passes completed by presented frames.
    pub wraps: u64,
}

/// A master reading resolved against the active range.
#[derive(Clone, Copy, Debug)]
struct Position {
    /// Offset from the active range start without wrapping (may be negative in reverse).
    unwrapped: i128,
    /// Loop pass (0 without looping).
    cycle: i128,
    /// Position tick (wrapped when looping).
    tick: Option<Ticks>,
}

fn overflow<T>(v: Option<T>) -> Result<T> {
    v.ok_or(PulseError::Overflow)
}

/// Frame clock and transport over one sequence.
#[derive(Clone, Debug)]
pub struct PlaybackScheduler {
    rate: FrameRate,
    range: TickRange,
    active: TickRange,
    in_out: Option<TickRange>,
    looping: bool,
    speed: PlaySpeed,
    policy: DropPolicy,
    transport: Transport,
    /// Playhead in sequence ticks (frame-aligned while paused).
    playhead: Ticks,
    anchor_samples: u64,
    anchor_tick: Ticks,
    last_frame: Option<u64>,
    /// Presentation index of `last_frame` relative to the current anchor.
    last_p: Option<i128>,
    stats: PlaybackStats,
}

impl PlaybackScheduler {
    /// Paused at the start of the play range (work area or whole sequence), 1x forward, no loop.
    pub fn new(sequence: &StudioSequence, policy: DropPolicy) -> Result<Self> {
        let rate = rate_of(sequence)?;
        let range = play_range(sequence)?;
        if range.is_empty() {
            return Err(PulseError::ZeroDuration);
        }
        Ok(Self {
            rate,
            range,
            active: range,
            in_out: None,
            looping: false,
            speed: PlaySpeed::FORWARD,
            policy,
            transport: Transport::Paused,
            playhead: range.start(),
            anchor_samples: 0,
            anchor_tick: range.start(),
            last_frame: None,
            last_p: None,
            stats: PlaybackStats::default(),
        })
    }
    pub fn rate(&self) -> FrameRate {
        self.rate
    }
    /// The play range (work area or whole sequence).
    pub fn range(&self) -> TickRange {
        self.range
    }
    /// The range being played: the play range, or the in/out span during play in-to-out.
    pub fn active_range(&self) -> TickRange {
        self.active
    }
    pub fn in_out(&self) -> Option<TickRange> {
        self.in_out
    }
    pub fn looping(&self) -> bool {
        self.looping
    }
    pub fn speed(&self) -> PlaySpeed {
        self.speed
    }
    pub fn policy(&self) -> DropPolicy {
        self.policy
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
    fn first_frame(&self, r: TickRange) -> u64 {
        self.frame_of(r.start())
    }
    fn last_frame_in(&self, r: TickRange) -> u64 {
        self.frame_of(Ticks::new(r.end().value().saturating_sub(1)))
    }
    fn last_frame_of_range(&self) -> u64 {
        self.last_frame_in(self.range)
    }
    /// Frames per pass of the active range.
    fn frames_per_pass(&self) -> Result<i128> {
        let n = self
            .last_frame_in(self.active)
            .checked_sub(self.first_frame(self.active))
            .and_then(|d| d.checked_add(1))
            .ok_or(PulseError::Overflow)?;
        Ok(i128::from(n))
    }

    /// Resolves the clock reading against the anchor, speed, active range and loop flag.
    fn position(&self, clock: &dyn AudioClock) -> Result<Position> {
        let played = clock
            .sample_position()
            .checked_sub(self.anchor_samples)
            .ok_or(PulseError::Underflow)?;
        let elapsed = i128::from(ticks_from_samples(played, clock.sample_rate_hz())?.value());
        let num = i128::from(self.speed.numerator.unsigned_abs());
        let den = i128::from(self.speed.denominator);
        let scaled = overflow(elapsed.checked_mul(num))?;
        let start = i128::from(self.active.start().value());
        let base = overflow(i128::from(self.anchor_tick.value()).checked_sub(start))?;
        let unwrapped = if self.speed.is_reverse() {
            // Ceil of the distance so the position is the exact (real) position rounded down.
            let distance = overflow(
                scaled
                    .checked_add(overflow(den.checked_sub(1))?)
                    .and_then(|v| v.checked_div(den)),
            )?;
            overflow(base.checked_sub(distance))?
        } else {
            overflow(base.checked_add(overflow(scaled.checked_div(den))?))?
        };
        let len = i128::from(self.active.duration().value());
        let (cycle, offset) = if self.looping {
            (
                overflow(unwrapped.checked_div_euclid(len))?,
                overflow(unwrapped.checked_rem_euclid(len))?,
            )
        } else {
            (0, unwrapped)
        };
        let tick = overflow(start.checked_add(offset))?;
        let tick = u64::try_from(tick).ok().map(Ticks::new);
        Ok(Position {
            unwrapped,
            cycle,
            tick,
        })
    }

    /// Sequence position of the master clock for the current reading.
    pub fn master(&self, clock: &dyn AudioClock) -> Result<Ticks> {
        Ok(self
            .position(clock)?
            .tick
            .unwrap_or(self.active.start()))
    }

    fn anchor(&mut self, clock: &dyn AudioClock) {
        self.anchor_samples = clock.sample_position();
        self.anchor_tick = self.playhead;
        let first = self.first_frame(self.active);
        self.last_p = self
            .last_frame
            .and_then(|f| f.checked_sub(first))
            .map(i128::from);
    }

    /// Starts (or resumes) playback of the play range from the playhead at the current speed.
    pub fn play(&mut self, clock: &dyn AudioClock) -> Result<()> {
        self.active = self.range;
        self.start_playing(clock)
    }

    fn start_playing(&mut self, clock: &dyn AudioClock) -> Result<()> {
        ticks_from_samples(1, clock.sample_rate_hz())?;
        let last = self.frame_start(self.last_frame_in(self.active))?;
        if self.speed.is_reverse() {
            if self.playhead <= self.active.start() || self.playhead >= self.active.end() {
                self.playhead = last.max(self.active.start());
            }
        } else if self.transport == Transport::Ended
            || self.playhead >= self.active.end()
            || self.playhead < self.active.start()
        {
            self.playhead = self.active.start();
        }
        self.last_frame = None;
        self.anchor(clock);
        self.transport = Transport::Playing;
        Ok(())
    }

    /// Sets the in point (first frame) and out point (last frame, inclusive) used by
    /// [`PlaybackScheduler::play_in_to_out`]: `invalid_range` when `out < in`, `outside_range` when
    /// the span leaves the play range.
    pub fn set_in_out(&mut self, in_frame: u64, out_frame: u64) -> Result<TickRange> {
        if out_frame < in_frame {
            return Err(PulseError::InvalidRange);
        }
        let start = self.frame_start(in_frame)?;
        let end = self.frame_start(overflow(out_frame.checked_add(1))?)?;
        if start < self.range.start() || end > self.range.end() {
            return Err(PulseError::OutsideRange);
        }
        let span = TickRange::new(start, end)?;
        self.in_out = Some(span);
        Ok(span)
    }
    pub fn clear_in_out(&mut self) {
        self.in_out = None;
    }

    /// Plays the in/out span from the in point at 1x forward (`cmd.transport.playintoout`); with
    /// looping on it repeats the span. `invalid_range` when no in/out is set.
    pub fn play_in_to_out(&mut self, clock: &dyn AudioClock) -> Result<()> {
        let span = self.in_out.ok_or(PulseError::InvalidRange)?;
        self.active = span;
        self.speed = PlaySpeed::FORWARD;
        self.playhead = span.start();
        self.transport = Transport::Paused;
        self.start_playing(clock)
    }

    /// Turns looping on or off; while playing the position is re-anchored where it is.
    pub fn set_looping(&mut self, looping: bool, clock: &dyn AudioClock) -> Result<()> {
        if self.transport == Transport::Playing {
            self.reanchor(clock)?;
        }
        self.looping = looping;
        Ok(())
    }

    fn reanchor(&mut self, clock: &dyn AudioClock) -> Result<()> {
        let pos = self.position(clock)?;
        let tick = pos.tick.unwrap_or(self.active.start());
        self.playhead = tick
            .max(self.active.start())
            .min(Ticks::new(self.active.end().value().saturating_sub(1)));
        self.anchor(clock);
        Ok(())
    }

    /// Changes the speed; while playing the position is re-anchored where it is, so the change is
    /// continuous. Paused, the speed applies at the next play.
    pub fn set_speed(&mut self, speed: PlaySpeed, clock: &dyn AudioClock) -> Result<()> {
        if self.transport == Transport::Playing {
            self.reanchor(clock)?;
        }
        self.speed = speed;
        Ok(())
    }

    /// `L` (`cmd.transport.shuttle.right`): from stop or reverse 1x forward, else double up to
    /// [`MAX_SHUTTLE`]. Returns the new speed.
    pub fn shuttle_right(&mut self, clock: &dyn AudioClock) -> Result<PlaySpeed> {
        self.shuttle(false, clock)
    }
    /// `J` (`cmd.transport.shuttle.left`): mirror of [`PlaybackScheduler::shuttle_right`].
    pub fn shuttle_left(&mut self, clock: &dyn AudioClock) -> Result<PlaySpeed> {
        self.shuttle(true, clock)
    }
    /// `K` (`cmd.transport.shuttle.stop`): pause.
    pub fn shuttle_stop(&mut self, clock: &dyn AudioClock) -> Result<()> {
        self.pause(clock)
    }
    /// `Shift+L` / `Shift+J`: slow shuttle at 1/[`SLOW_SHUTTLE_DENOMINATOR`] speed.
    pub fn shuttle_slow(&mut self, reverse: bool, clock: &dyn AudioClock) -> Result<PlaySpeed> {
        let n: i32 = if reverse { -1 } else { 1 };
        let speed = PlaySpeed::new(n, SLOW_SHUTTLE_DENOMINATOR)?;
        self.go(speed, clock)?;
        Ok(speed)
    }

    fn shuttle(&mut self, reverse: bool, clock: &dyn AudioClock) -> Result<PlaySpeed> {
        let playing = self.transport == Transport::Playing;
        let same_way = playing && self.speed.is_reverse() == reverse && self.speed.denominator == 1;
        let magnitude = if same_way {
            self.speed
                .numerator
                .unsigned_abs()
                .saturating_mul(2)
                .min(MAX_SHUTTLE)
        } else {
            1
        };
        let signed = i32::try_from(magnitude).map_err(|_| PulseError::InvalidSpeed)?;
        let speed = PlaySpeed::new(if reverse { signed.saturating_neg() } else { signed }, 1)?;
        self.go(speed, clock)?;
        Ok(speed)
    }

    fn go(&mut self, speed: PlaySpeed, clock: &dyn AudioClock) -> Result<()> {
        if self.transport == Transport::Playing {
            self.set_speed(speed, clock)
        } else {
            self.speed = speed;
            self.start_playing(clock)
        }
    }

    /// Pauses on the frame on screen (or the due frame when none was presented yet).
    pub fn pause(&mut self, clock: &dyn AudioClock) -> Result<()> {
        if self.transport == Transport::Playing {
            let frame = match self.last_frame {
                Some(f) => f,
                None => self.frame_of(
                    self.master(clock)?
                        .min(Ticks::new(self.active.end().value().saturating_sub(1))),
                ),
            };
            let frame = frame
                .min(self.last_frame_in(self.active))
                .max(self.first_frame(self.active));
            self.playhead = self.frame_start(frame)?.max(self.active.start());
        }
        self.transport = Transport::Paused;
        Ok(())
    }

    /// Moves the playhead to the frame holding `tick`, clamped into the active range; while
    /// playing the master is re-anchored there.
    pub fn scrub(&mut self, tick: Ticks, clock: &dyn AudioClock) -> Result<()> {
        let r = if self.transport == Transport::Playing {
            self.active
        } else {
            self.range
        };
        let clamped = tick
            .max(r.start())
            .min(Ticks::new(r.end().value().saturating_sub(1)));
        self.playhead = self.frame_start(self.frame_of(clamped))?.max(r.start());
        if self.transport == Transport::Playing {
            self.last_frame = None;
            self.anchor(clock);
        } else {
            self.transport = Transport::Paused;
        }
        Ok(())
    }

    /// Step forward (`frames > 0`) or back while paused (pauses first), clamped into the play
    /// range.
    pub fn step(&mut self, frames: i64, clock: &dyn AudioClock) -> Result<Ticks> {
        self.pause(clock)?;
        self.active = self.range;
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

    /// Presentation index -> (cycle, frame).
    fn frame_at(&self, p: i128) -> Result<(i128, u64)> {
        let n = self.frames_per_pass()?;
        let cycle = overflow(p.checked_div_euclid(n))?;
        let within = overflow(p.checked_rem_euclid(n))?;
        let frame = u64::try_from(overflow(
            i128::from(self.first_frame(self.active)).checked_add(within),
        )?)
        .map_err(|_| PulseError::Overflow)?;
        Ok((cycle, frame))
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
        let pos = self.position(clock)?;
        let reverse = self.speed.is_reverse();
        let len = i128::from(self.active.duration().value());
        let first = self.first_frame(self.active);
        let n = self.frames_per_pass()?;
        let past_end = !self.looping && if reverse { pos.unwrapped < 0 } else { pos.unwrapped >= len };
        if past_end {
            // Frames passed before the edge without being shown count as dropped.
            let edge = if reverse { 0 } else { overflow(n.checked_sub(1))? };
            let dropped = match (self.policy, self.last_p) {
                (DropPolicy::DropLate, Some(last)) => {
                    u64::try_from(overflow(edge.checked_sub(last))?.unsigned_abs()).unwrap_or(0)
                }
                _ => 0,
            };
            self.stats.dropped = self.stats.dropped.saturating_add(dropped);
            self.transport = Transport::Ended;
            self.playhead = if reverse {
                self.active.start()
            } else {
                self.active.end()
            };
            let master = match pos.tick {
                Some(t) if !reverse => t,
                _ => self.active.start(),
            };
            return Ok(FrameDecision {
                action: FrameAction::End,
                master,
                dropped,
            });
        }
        let tick = pos.tick.ok_or(PulseError::Underflow)?;
        let due_frame = self.frame_of(tick);
        let within = i128::from(due_frame.checked_sub(first).ok_or(PulseError::Underflow)?);
        let due_p = overflow(overflow(pos.cycle.checked_mul(n))?.checked_add(within))?;
        let p = match (self.policy, self.last_p) {
            (_, Some(last)) if (!reverse && last >= due_p) || (reverse && last <= due_p) => {
                return Ok(FrameDecision {
                    action: FrameAction::Hold,
                    master: tick,
                    dropped: 0,
                });
            }
            (DropPolicy::DropLate, _) | (_, None) => due_p,
            (DropPolicy::PresentEveryFrame, Some(last)) => {
                overflow(last.checked_add(if reverse { -1 } else { 1 }))?
            }
        };
        let dropped = match self.last_p {
            Some(last) => u64::try_from(overflow(p.checked_sub(last))?.unsigned_abs())
                .unwrap_or(u64::MAX)
                .saturating_sub(1),
            None => 0,
        };
        let (cycle, frame) = self.frame_at(p)?;
        let frame_tick = self.frame_start(frame)?;
        if let Some(last) = self.last_p
            && self.frame_at(last)?.0 != cycle
        {
            self.stats.wraps = self.stats.wraps.saturating_add(1);
        }
        self.last_frame = Some(frame);
        self.last_p = Some(p);
        self.playhead = frame_tick.max(self.active.start());
        self.stats.presented = self.stats.presented.saturating_add(1);
        self.stats.dropped = self.stats.dropped.saturating_add(dropped);
        // Lag in sequence ticks: whole frames between the due and the presented frame plus the
        // distance into the due frame (forward) or to its end (reverse).
        let tpf = i128::from(self.rate.ticks_per_frame());
        let due_start = i128::from(self.frame_start(due_frame)?.value());
        let at = i128::from(tick.value());
        let intra = if reverse {
            overflow(overflow(due_start.checked_add(tpf))?.checked_sub(at))?
        } else {
            overflow(at.checked_sub(due_start))?
        };
        let behind = overflow(due_p.checked_sub(p))?;
        let behind = if reverse { overflow(behind.checked_neg())? } else { behind };
        if behind < 0 {
            self.stats.leads = self.stats.leads.saturating_add(1);
        } else {
            let lag = overflow(overflow(behind.checked_mul(tpf))?.checked_add(intra))?;
            self.stats.max_lag_ticks = self
                .stats
                .max_lag_ticks
                .max(u64::try_from(lag).unwrap_or(u64::MAX));
        }
        Ok(FrameDecision {
            action: FrameAction::Present {
                frame,
                tick: frame_tick,
            },
            master: tick,
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
