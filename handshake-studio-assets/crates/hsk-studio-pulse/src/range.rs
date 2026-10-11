//! Signed tick offsets, half-open tick ranges and checked trim/ripple over an ordered lane of
//! ranges (STU-VID-026 time arithmetic). Pure time algebra: no ids, clips, tracks or documents.
//! All arithmetic is checked; inputs are never modified (callers get a new `Vec`).
use crate::error::{PulseError, Result};
use hsk_studio_accord::{CancellationToken, FrameRate, Ticks};

pub const MAX_RANGES: usize = 262_144;
const CANCEL_STRIDE: usize = 256;

/// Signed tick offset; magnitude never exceeds `u64::MAX`, so `Ticks + delta` is decidable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickDelta(i128);

impl TickDelta {
    pub const ZERO: TickDelta = TickDelta(0);

    pub fn new(value: i128) -> Result<Self> {
        if value.unsigned_abs() > u128::from(u64::MAX) {
            return Err(PulseError::Overflow);
        }
        Ok(Self(value))
    }
    /// Offset that moves `from` to `to`.
    pub fn between(from: Ticks, to: Ticks) -> Self {
        Self(i128::from(to.value()) - i128::from(from.value()))
    }
    pub fn frames(frames: i64, rate: FrameRate) -> Result<Self> {
        Self::new(i128::from(frames) * i128::from(rate.ticks_per_frame()))
    }
    pub fn value(self) -> i128 {
        self.0
    }
    pub fn negated(self) -> Self {
        Self(-self.0)
    }
    pub fn is_whole_frames(self, rate: FrameRate) -> bool {
        self.0 % i128::from(rate.ticks_per_frame()) == 0
    }
}

/// `ticks + delta`; below zero is `Underflow`, above `u64::MAX` is `Overflow`.
pub fn offset(ticks: Ticks, delta: TickDelta) -> Result<Ticks> {
    let value = i128::from(ticks.value()) + delta.0;
    if value < 0 {
        return Err(PulseError::Underflow);
    }
    u64::try_from(value)
        .map(Ticks::new)
        .map_err(|_| PulseError::Overflow)
}

/// Where edit points may land: anywhere (audio, captions) or on whole frames (video).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grid {
    Free,
    Frames(FrameRate),
}

impl Grid {
    fn check(self, delta: TickDelta) -> Result<()> {
        match self {
            Self::Frames(rate) if !delta.is_whole_frames(rate) => Err(PulseError::NotFrameAligned),
            _ => Ok(()),
        }
    }
}

/// Half-open `[start, end)` in ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickRange {
    start: Ticks,
    end: Ticks,
}

impl TickRange {
    pub fn new(start: Ticks, end: Ticks) -> Result<Self> {
        if start.value() > end.value() {
            return Err(PulseError::InvalidRange);
        }
        Ok(Self { start, end })
    }
    pub fn from_start_duration(start: Ticks, duration: Ticks) -> Result<Self> {
        Self::new(start, start.checked_add(duration)?)
    }
    pub fn start(self) -> Ticks {
        self.start
    }
    pub fn end(self) -> Ticks {
        self.end
    }
    pub fn duration(self) -> Ticks {
        Ticks::new(self.end.value() - self.start.value())
    }
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
    pub fn contains(self, t: Ticks) -> bool {
        self.start.value() <= t.value() && t.value() < self.end.value()
    }
    pub fn overlaps(self, other: TickRange) -> bool {
        self.start.value() < other.end.value() && other.start.value() < self.end.value()
    }
    /// `self` ends exactly where `other` starts.
    pub fn abuts(self, other: TickRange) -> bool {
        self.end == other.start
    }
    pub fn intersect(self, other: TickRange) -> Option<TickRange> {
        let start = self.start.value().max(other.start.value());
        let end = self.end.value().min(other.end.value());
        (start < end).then(|| Self {
            start: Ticks::new(start),
            end: Ticks::new(end),
        })
    }
    pub fn shifted(self, delta: TickDelta) -> Result<TickRange> {
        Ok(Self {
            start: offset(self.start, delta)?,
            end: offset(self.end, delta)?,
        })
    }
    /// Splits at an interior point; both halves are non-empty.
    pub fn split_at(self, at: Ticks) -> Result<(TickRange, TickRange)> {
        if at.value() <= self.start.value() || at.value() >= self.end.value() {
            return Err(PulseError::OutsideRange);
        }
        Ok((
            Self {
                start: self.start,
                end: at,
            },
            Self {
                start: at,
                end: self.end,
            },
        ))
    }
    /// Keeps the start, moves the end so the range lasts `new_duration` (> 0).
    pub fn trim_end_to(self, new_duration: Ticks, grid: Grid) -> Result<TickRange> {
        if new_duration.value() == 0 {
            return Err(PulseError::ZeroDuration);
        }
        let end = self.start.checked_add(new_duration)?;
        grid.check(TickDelta::between(self.end, end))?;
        Ok(Self {
            start: self.start,
            end,
        })
    }
    /// Keeps the end, moves the start to `new_start` (< end).
    pub fn trim_start_to(self, new_start: Ticks, grid: Grid) -> Result<TickRange> {
        match new_start.value().cmp(&self.end.value()) {
            std::cmp::Ordering::Equal => return Err(PulseError::ZeroDuration),
            std::cmp::Ordering::Greater => return Err(PulseError::OutsideRange),
            std::cmp::Ordering::Less => {}
        }
        grid.check(TickDelta::between(self.start, new_start))?;
        Ok(Self {
            start: new_start,
            end: self.end,
        })
    }
}

/// A lane is ordered by start and non-overlapping (adjacent ranges may abut).
pub fn validate_lane(ranges: &[TickRange], cancel: &CancellationToken) -> Result<()> {
    cancel.check()?;
    if ranges.len() > MAX_RANGES {
        return Err(PulseError::LimitExceeded);
    }
    for (i, pair) in ranges.windows(2).enumerate() {
        if i % CANCEL_STRIDE == 0 {
            cancel.check()?;
        }
        if pair[1].start.value() < pair[0].start.value() {
            return Err(PulseError::UnorderedRanges);
        }
        if pair[1].start.value() < pair[0].end.value() {
            return Err(PulseError::Overlap);
        }
    }
    Ok(())
}

fn index_of(ranges: &[TickRange], index: usize) -> Result<TickRange> {
    ranges.get(index).copied().ok_or(PulseError::OutsideRange)
}

fn shift_tail(out: &mut [TickRange], delta: TickDelta, cancel: &CancellationToken) -> Result<()> {
    for (i, range) in out.iter_mut().enumerate() {
        if i % CANCEL_STRIDE == 0 {
            cancel.check()?;
        }
        *range = range.shifted(delta)?;
    }
    Ok(())
}

/// Shifts every range starting at or after `from` by `delta`. A range straddling `from` stays,
/// so a negative shift that would collide with it is refused (`overlap`).
pub fn ripple_shift(
    ranges: &[TickRange],
    from: Ticks,
    delta: TickDelta,
    cancel: &CancellationToken,
) -> Result<Vec<TickRange>> {
    validate_lane(ranges, cancel)?;
    let first = ranges.partition_point(|r| r.start.value() < from.value());
    let mut out = ranges.to_vec();
    shift_tail(&mut out[first..], delta, cancel)?;
    if first > 0 && first < out.len() && out[first].start.value() < out[first - 1].end.value() {
        return Err(PulseError::Overlap);
    }
    Ok(out)
}

/// Moves the tail of `ranges[index]` so it lasts `new_duration`; nothing else moves, a gap opens
/// or the range extends into free space (`overlap` if it would run into the next range).
pub fn trim_end(
    ranges: &[TickRange],
    index: usize,
    new_duration: Ticks,
    grid: Grid,
    cancel: &CancellationToken,
) -> Result<Vec<TickRange>> {
    validate_lane(ranges, cancel)?;
    let trimmed = index_of(ranges, index)?.trim_end_to(new_duration, grid)?;
    if ranges
        .get(index + 1)
        .is_some_and(|next| trimmed.end.value() > next.start.value())
    {
        return Err(PulseError::Overlap);
    }
    let mut out = ranges.to_vec();
    out[index] = trimmed;
    Ok(out)
}

/// Moves the head of `ranges[index]` to `new_start`; the end stays.
pub fn trim_start(
    ranges: &[TickRange],
    index: usize,
    new_start: Ticks,
    grid: Grid,
    cancel: &CancellationToken,
) -> Result<Vec<TickRange>> {
    validate_lane(ranges, cancel)?;
    let trimmed = index_of(ranges, index)?.trim_start_to(new_start, grid)?;
    if index > 0 && trimmed.start.value() < ranges[index - 1].end.value() {
        return Err(PulseError::Overlap);
    }
    let mut out = ranges.to_vec();
    out[index] = trimmed;
    Ok(out)
}

/// Ripple trim of the tail: `ranges[index]` lasts `new_duration` and every later range shifts by
/// the duration change, so no gap opens and nothing collides.
pub fn ripple_trim_end(
    ranges: &[TickRange],
    index: usize,
    new_duration: Ticks,
    grid: Grid,
    cancel: &CancellationToken,
) -> Result<Vec<TickRange>> {
    validate_lane(ranges, cancel)?;
    let original = index_of(ranges, index)?;
    let trimmed = original.trim_end_to(new_duration, grid)?;
    let delta = TickDelta::between(original.end, trimmed.end);
    let mut out = ranges.to_vec();
    out[index] = trimmed;
    shift_tail(&mut out[index + 1..], delta, cancel)?;
    Ok(out)
}

/// Ripple trim of the head: `new_start` is the equivalent plain-trim head position. The range
/// keeps its start, lasts `duration - (new_start - start)`, and later ranges shift by the
/// duration change (a head extension, `new_start < start`, pushes them later).
pub fn ripple_trim_start(
    ranges: &[TickRange],
    index: usize,
    new_start: Ticks,
    grid: Grid,
    cancel: &CancellationToken,
) -> Result<Vec<TickRange>> {
    validate_lane(ranges, cancel)?;
    let original = index_of(ranges, index)?;
    match new_start.value().cmp(&original.end.value()) {
        std::cmp::Ordering::Equal => return Err(PulseError::ZeroDuration),
        std::cmp::Ordering::Greater => return Err(PulseError::OutsideRange),
        std::cmp::Ordering::Less => {}
    }
    let moved = TickDelta::between(original.start, new_start);
    grid.check(moved)?;
    let change = moved.negated();
    let trimmed = TickRange {
        start: original.start,
        end: offset(original.end, change)?,
    };
    let mut out = ranges.to_vec();
    out[index] = trimmed;
    shift_tail(&mut out[index + 1..], change, cancel)?;
    Ok(out)
}
