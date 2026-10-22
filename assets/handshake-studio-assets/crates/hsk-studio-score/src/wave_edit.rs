//! Waveform (sample) editor operations on planar audio (lane SMB-AU-WAVEFORM-EDITOR, parity row
//! AU-20). A [`WaveBuffer`] holds one f32 slice per channel at one sample rate; every operation is
//! sample-accurate (positions are sample indices, ranges are half-open `[start, end)`), checks every
//! argument before it changes anything (a refused operation leaves the buffer bit for bit as it
//! was) and returns a [`WaveEditReport`].
//!
//! * **Selections** carry a channel mask ([`WaveSelection::channels`], bit `i` = channel `i`,
//!   [`ALL_CHANNELS`] = every channel): edits that keep the length (silence, fades, gain envelope,
//!   amplify, normalise, invert, reverse, paste overwrite / mix) apply to the masked channels only;
//!   edits that change the length (delete, cut, insert silence, paste insert) apply to every
//!   channel, so the channels stay equally long.
//! * **Zero crossings.** Boundary `b` (between samples `b - 1` and `b`, `0 ..= frames`) is a zero
//!   crossing of a channel when `b` is `0` or `frames`, or `x[b-1] * x[b] <= 0` (a sign change or
//!   an exact zero), and of a mask when it is one of every masked channel.
//!   [`WaveBuffer::snap_to_zero_crossing`] searches at most `max_distance` samples (nearest: ties go
//!   earlier); [`WaveBuffer::snap_selection`] moves both ends inward or outward.
//! * **Fades** reuse score's [`FadeCurve`] shapes over the selection length `n`: frame `k` of a fade
//!   in gains `Fade{n, curve}.in_gain(k)` (exactly 0 at the first frame); a fade out is its mirror,
//!   frame `k` gains `in_gain(n - 1 - k)` (exactly 0 at the last frame), so a fade out of a reversed
//!   signal is the reversed fade in.
//! * **Gain envelope**: points `(offset from the selection start, dB)` with strictly increasing
//!   offsets `0 ..= n`; the gain is linear in dB between points and held before the first and after
//!   the last point; `10^(dB/20)` in f64.
//! * **Amplify** by `dB` and **normalise** the selection's sample peak to `target dBFS`, linked
//!   (one gain for all masked channels, from their largest peak) or per channel; a silent channel
//!   (peak 0) is left unchanged (its gain reported as `None`).
//!
//! Results are checked before writing: a gain or mix that would leave the finite f32 range is
//! refused with `NotFinite` and nothing changes. Length growth is bounded by [`MAX_WAVE_FRAMES`] and
//! reserved fallibly first (`CapacityExceeded`). Cancellation is observed before any write and
//! between bounded chunks of the read-only scans; writes are not interrupted (bounded by the
//! buffer size). Audition behaviour parity (selection snapping rules, fade shapes, normalise
//! options) is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use crate::error::ScoreError;
use crate::layout::{MAX_CHANNELS, ticks_per_sample};
use crate::session::{Fade, FadeCurve};
use hsk_studio_accord::CancellationToken;

/// Longest channel in samples (authored bound: about 6.2 hours at 48 kHz).
pub const MAX_WAVE_FRAMES: u64 = 1 << 30;
/// Channel mask naming every channel of the buffer.
pub const ALL_CHANNELS: u32 = u32::MAX;
/// Amplify range in dB.
pub const WAVE_AMPLIFY_DB: (f64, f64) = (-96.0, 48.0);
/// Gain envelope point range in dB.
pub const WAVE_ENVELOPE_DB: (f64, f64) = (-96.0, 24.0);
/// Normalise target range in dBFS.
pub const WAVE_NORMALIZE_DBFS: (f64, f64) = (-96.0, 0.0);
/// Most gain envelope points.
pub const MAX_WAVE_ENVELOPE_POINTS: usize = 4096;
/// Frames between cancellation checks of read-only scans.
const SCAN_CHUNK: usize = 1 << 16;

/// A time range of some channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaveSelection {
    pub start: u64,
    pub end: u64,
    /// Bit `i` selects channel `i`; [`ALL_CHANNELS`] selects every channel.
    pub channels: u32,
}

impl WaveSelection {
    /// `start .. end` of every channel.
    pub const fn all(start: u64, end: u64) -> Self {
        Self { start, end, channels: ALL_CHANNELS }
    }
}

/// Copied audio (one slice per channel, equal lengths).
#[derive(Clone, Debug, PartialEq)]
pub struct WaveClip {
    pub channels: Vec<Vec<f32>>,
}

impl WaveClip {
    pub fn frames(&self) -> u64 {
        self.channels.first().map_or(0, |c| c.len() as u64)
    }
}

/// How a clip is pasted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteMode {
    /// Inserted at the position into every channel (the file grows; the clip has one slice per
    /// channel).
    Insert,
    /// Replaces the masked channels from the position (the clip has one slice per masked channel and
    /// must fit inside the buffer).
    Overwrite,
    /// Added to the masked channels from the position (as `Overwrite`).
    Mix,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FadeDirection {
    In,
    Out,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapDirection {
    Nearest,
    Earlier,
    Later,
}

/// What an edit did.
#[derive(Clone, Debug, PartialEq)]
pub struct WaveEditReport {
    pub frames_before: u64,
    pub frames_after: u64,
    /// Samples `[changed_start, changed_end)` of the result that may differ from the input at the same
    /// index (for length-changing edits: from the edit point to the new end).
    pub changed_start: u64,
    pub changed_end: u64,
    /// Gain per masked channel in dB (amplify, normalise), `None` for a channel left unchanged.
    pub gains_db: Vec<Option<f64>>,
}

/// Planar audio being edited.
#[derive(Clone, Debug, PartialEq)]
pub struct WaveBuffer {
    sample_rate_hz: u32,
    channels: Vec<Vec<f32>>,
}

fn to_index(v: u64) -> Result<usize, ScoreError> {
    usize::try_from(v).map_err(|_| ScoreError::CapacityExceeded)
}

fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

fn check_db(db: f64, range: (f64, f64)) -> Result<(), ScoreError> {
    if !db.is_finite() {
        return Err(ScoreError::NotFinite);
    }
    if db < range.0 || db > range.1 {
        return Err(ScoreError::OutOfRange);
    }
    Ok(())
}

fn peak(x: &[f32], cancel: &CancellationToken) -> Result<f32, ScoreError> {
    let mut p = 0.0_f32;
    for chunk in x.chunks(SCAN_CHUNK) {
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        p = chunk.iter().fold(p, |m, v| m.max(v.abs()));
    }
    Ok(p)
}

fn crossing(x: &[f32], b: usize) -> bool {
    match (b.checked_sub(1).and_then(|i| x.get(i)), x.get(b)) {
        (Some(a), Some(c)) => f64::from(*a) * f64::from(*c) <= 0.0,
        _ => true,
    }
}

impl WaveBuffer {
    /// Refusals: rate not admitted -> `InvalidRate`; no channel or more than `MAX_CHANNELS` ->
    /// `ChannelMismatch`; unequal lengths -> `LengthMismatch`; more than [`MAX_WAVE_FRAMES`] ->
    /// `CapacityExceeded`; a NaN/inf sample -> `NotFinite`.
    pub fn new(sample_rate_hz: u32, channels: Vec<Vec<f32>>) -> Result<Self, ScoreError> {
        ticks_per_sample(sample_rate_hz)?;
        if channels.is_empty() || channels.len() > MAX_CHANNELS {
            return Err(ScoreError::ChannelMismatch);
        }
        let frames = channels[0].len();
        if channels.iter().any(|c| c.len() != frames) {
            return Err(ScoreError::LengthMismatch);
        }
        if frames as u64 > MAX_WAVE_FRAMES {
            return Err(ScoreError::CapacityExceeded);
        }
        if channels.iter().flatten().any(|v| !v.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        Ok(Self { sample_rate_hz, channels })
    }
    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }
    pub fn frames(&self) -> u64 {
        self.channels[0].len() as u64
    }
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }
    pub fn channels(&self) -> &[Vec<f32>] {
        &self.channels
    }
    pub fn into_channels(self) -> Vec<Vec<f32>> {
        self.channels
    }

    /// Indices of a mask: [`ALL_CHANNELS`] or a non-empty subset of the channels, else
    /// `ChannelMismatch`.
    pub fn mask_channels(&self, mask: u32) -> Result<Vec<usize>, ScoreError> {
        let n = self.channels.len();
        if mask == ALL_CHANNELS {
            return Ok((0..n).collect());
        }
        let full = 1u32.checked_shl(n as u32).map_or(u32::MAX, |b| b.wrapping_sub(1));
        if mask == 0 || mask & !full != 0 {
            return Err(ScoreError::ChannelMismatch);
        }
        Ok((0..n).filter(|i| 1u32.checked_shl(*i as u32).is_some_and(|b| mask & b != 0)).collect())
    }

    fn range(&self, start: u64, end: u64) -> Result<(usize, usize), ScoreError> {
        if start >= end || end > self.frames() {
            return Err(ScoreError::OutOfRange);
        }
        Ok((to_index(start)?, to_index(end)?))
    }

    fn selection(&self, sel: WaveSelection) -> Result<(usize, usize, Vec<usize>), ScoreError> {
        let (s, e) = self.range(sel.start, sel.end)?;
        Ok((s, e, self.mask_channels(sel.channels)?))
    }

    fn same_length(&self, sel: WaveSelection, gains_db: Vec<Option<f64>>) -> WaveEditReport {
        WaveEditReport { frames_before: self.frames(), frames_after: self.frames(), changed_start: sel.start, changed_end: sel.end, gains_db }
    }

    /// Reserves `extra` frames in every channel (fallible) after checking the bound.
    fn grow(&mut self, extra: u64) -> Result<(), ScoreError> {
        let total = self.frames().checked_add(extra).ok_or(ScoreError::CapacityExceeded)?;
        if total > MAX_WAVE_FRAMES {
            return Err(ScoreError::CapacityExceeded);
        }
        let extra = to_index(extra)?;
        for c in &mut self.channels {
            c.try_reserve(extra).map_err(|_| ScoreError::CapacityExceeded)?;
        }
        Ok(())
    }

    /// Whether boundary `b` is a zero crossing of every masked channel (module docs).
    pub fn is_zero_crossing(&self, b: u64, mask: u32) -> Result<bool, ScoreError> {
        if b > self.frames() {
            return Err(ScoreError::OutOfRange);
        }
        let b = to_index(b)?;
        Ok(self.mask_channels(mask)?.iter().all(|c| crossing(&self.channels[*c], b)))
    }

    /// The zero crossing of the masked channels nearest to boundary `position` in `direction`, at most
    /// `max_distance` samples away (`None` when there is none). Refusals: position past the end ->
    /// `OutOfRange`; bad mask -> `ChannelMismatch`; cancel (per chunk) -> `Canceled`.
    pub fn snap_to_zero_crossing(
        &self,
        position: u64,
        mask: u32,
        direction: SnapDirection,
        max_distance: u64,
        cancel: &CancellationToken,
    ) -> Result<Option<u64>, ScoreError> {
        let len = self.frames();
        if position > len {
            return Err(ScoreError::OutOfRange);
        }
        let channels = self.mask_channels(mask)?;
        let hit = |b: u64| -> bool { usize::try_from(b).is_ok_and(|b| channels.iter().all(|c| crossing(&self.channels[*c], b))) };
        let reach = max_distance.min(len);
        let mut d = 0u64;
        loop {
            if d.checked_rem(SCAN_CHUNK as u64) == Some(0) {
                cancel.check().map_err(|_| ScoreError::Canceled)?;
            }
            let earlier = position.checked_sub(d);
            let later = position.checked_add(d).filter(|b| *b <= len);
            match direction {
                SnapDirection::Nearest | SnapDirection::Earlier => {
                    if let Some(b) = earlier.filter(|b| hit(*b)) {
                        return Ok(Some(b));
                    }
                }
                SnapDirection::Later => {}
            }
            if matches!(direction, SnapDirection::Nearest | SnapDirection::Later)
                && let Some(b) = later.filter(|b| hit(*b))
            {
                return Ok(Some(b));
            }
            if d >= reach {
                return Ok(None);
            }
            d = d.saturating_add(1);
        }
    }

    /// Moves the selection's start and end to zero crossings of its channels: `inward` moves the
    /// start later and the end earlier, otherwise the start earlier and the end later. `None` when an
    /// end finds no crossing within `max_distance` or the selection would become empty.
    pub fn snap_selection(&self, sel: WaveSelection, inward: bool, max_distance: u64, cancel: &CancellationToken) -> Result<Option<WaveSelection>, ScoreError> {
        self.selection(sel)?;
        let (s_dir, e_dir) = if inward { (SnapDirection::Later, SnapDirection::Earlier) } else { (SnapDirection::Earlier, SnapDirection::Later) };
        let start = self.snap_to_zero_crossing(sel.start, sel.channels, s_dir, max_distance, cancel)?;
        let end = self.snap_to_zero_crossing(sel.end, sel.channels, e_dir, max_distance, cancel)?;
        Ok(match (start, end) {
            (Some(start), Some(end)) if start < end => Some(WaveSelection { start, end, channels: sel.channels }),
            _ => None,
        })
    }

    /// A copy of the selection (one slice per masked channel).
    pub fn copy(&self, sel: WaveSelection) -> Result<WaveClip, ScoreError> {
        let (s, e, chans) = self.selection(sel)?;
        Ok(WaveClip { channels: chans.iter().map(|c| self.channels[*c][s..e].to_vec()).collect() })
    }

    /// Removes `start .. end` from every channel.
    pub fn delete(&mut self, start: u64, end: u64, cancel: &CancellationToken) -> Result<WaveEditReport, ScoreError> {
        let (s, e) = self.range(start, end)?;
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        let before = self.frames();
        for c in &mut self.channels {
            c.drain(s..e);
        }
        Ok(WaveEditReport { frames_before: before, frames_after: self.frames(), changed_start: start, changed_end: self.frames(), gains_db: Vec::new() })
    }

    /// Copies then deletes `start .. end` of every channel.
    pub fn cut(&mut self, start: u64, end: u64, cancel: &CancellationToken) -> Result<(WaveClip, WaveEditReport), ScoreError> {
        let clip = self.copy(WaveSelection::all(start, end))?;
        let report = self.delete(start, end, cancel)?;
        Ok((clip, report))
    }

    /// Inserts `frames` (>= 1) zero samples at boundary `at` of every channel.
    pub fn insert_silence(&mut self, at: u64, frames: u64, cancel: &CancellationToken) -> Result<WaveEditReport, ScoreError> {
        if at > self.frames() || frames == 0 {
            return Err(ScoreError::OutOfRange);
        }
        let a = to_index(at)?;
        let n = to_index(frames)?;
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        let before = self.frames();
        self.grow(frames)?;
        for c in &mut self.channels {
            c.splice(a..a, std::iter::repeat_n(0.0, n));
        }
        Ok(WaveEditReport { frames_before: before, frames_after: self.frames(), changed_start: at, changed_end: self.frames(), gains_db: Vec::new() })
    }

    /// Pastes `clip` at boundary `at` (module docs and [`PasteMode`]); `mask` names the channels of an
    /// overwrite or mix (ignored by insert). Refusals: clip without frames, channel count not matching
    /// -> `ChannelMismatch`; unequal clip lengths -> `LengthMismatch`; NaN/inf clip sample or a mix
    /// leaving the f32 range -> `NotFinite`; position past the end or an overwrite / mix past the end
    /// -> `OutOfRange`; growth past [`MAX_WAVE_FRAMES`] -> `CapacityExceeded`.
    pub fn paste(&mut self, at: u64, clip: &WaveClip, mode: PasteMode, mask: u32, cancel: &CancellationToken) -> Result<WaveEditReport, ScoreError> {
        let frames = clip.frames();
        if clip.channels.iter().any(|c| c.len() as u64 != frames) {
            return Err(ScoreError::LengthMismatch);
        }
        if frames == 0 {
            return Err(ScoreError::OutOfRange);
        }
        if clip.channels.iter().flatten().any(|v| !v.is_finite()) {
            return Err(ScoreError::NotFinite);
        }
        if at > self.frames() {
            return Err(ScoreError::OutOfRange);
        }
        let a = to_index(at)?;
        let before = self.frames();
        match mode {
            PasteMode::Insert => {
                if clip.channels.len() != self.channels.len() {
                    return Err(ScoreError::ChannelMismatch);
                }
                cancel.check().map_err(|_| ScoreError::Canceled)?;
                self.grow(frames)?;
                for (c, src) in self.channels.iter_mut().zip(&clip.channels) {
                    c.splice(a..a, src.iter().copied());
                }
                let end = at.checked_add(frames).ok_or(ScoreError::CapacityExceeded)?;
                Ok(WaveEditReport { frames_before: before, frames_after: self.frames(), changed_start: at, changed_end: self.frames().max(end), gains_db: Vec::new() })
            }
            PasteMode::Overwrite | PasteMode::Mix => {
                let chans = self.mask_channels(mask)?;
                if chans.len() != clip.channels.len() {
                    return Err(ScoreError::ChannelMismatch);
                }
                let end = at.checked_add(frames).filter(|e| *e <= before).ok_or(ScoreError::OutOfRange)?;
                let e = to_index(end)?;
                if mode == PasteMode::Mix {
                    for (c, src) in chans.iter().zip(&clip.channels) {
                        cancel.check().map_err(|_| ScoreError::Canceled)?;
                        if self.channels[*c][a..e].iter().zip(src).any(|(x, y)| !(x + y).is_finite()) {
                            return Err(ScoreError::NotFinite);
                        }
                    }
                }
                cancel.check().map_err(|_| ScoreError::Canceled)?;
                for (c, src) in chans.iter().zip(&clip.channels) {
                    let dst = &mut self.channels[*c][a..e];
                    if mode == PasteMode::Mix {
                        for (x, y) in dst.iter_mut().zip(src) {
                            *x += *y;
                        }
                    } else {
                        dst.copy_from_slice(src);
                    }
                }
                Ok(WaveEditReport { frames_before: before, frames_after: before, changed_start: at, changed_end: end, gains_db: Vec::new() })
            }
        }
    }

    /// Sets the selection to exact zeros.
    pub fn silence(&mut self, sel: WaveSelection, cancel: &CancellationToken) -> Result<WaveEditReport, ScoreError> {
        let (s, e, chans) = self.selection(sel)?;
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        for c in chans {
            self.channels[c][s..e].fill(0.0);
        }
        Ok(self.same_length(sel, Vec::new()))
    }

    /// Fades the selection in or out with `curve` (module docs).
    pub fn fade(&mut self, sel: WaveSelection, direction: FadeDirection, curve: FadeCurve, cancel: &CancellationToken) -> Result<WaveEditReport, ScoreError> {
        let (s, e, chans) = self.selection(sel)?;
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        let n = e.saturating_sub(s);
        let fade = Fade { frames: n as u64, curve };
        let last = n.saturating_sub(1);
        for c in chans {
            for (k, v) in self.channels[c][s..e].iter_mut().enumerate() {
                let at = match direction {
                    FadeDirection::In => k,
                    FadeDirection::Out => last.saturating_sub(k),
                };
                *v = (f64::from(*v) * fade.in_gain(at as u64)) as f32;
            }
        }
        Ok(self.same_length(sel, Vec::new()))
    }

    /// Applies a gain envelope (module docs). Refusals: no points, more than
    /// [`MAX_WAVE_ENVELOPE_POINTS`], offsets not strictly increasing or past the selection length, dB
    /// outside [`WAVE_ENVELOPE_DB`] -> `OutOfRange`; NaN/inf dB or a result leaving the f32 range ->
    /// `NotFinite`.
    pub fn gain_envelope(&mut self, sel: WaveSelection, points: &[(u64, f64)], cancel: &CancellationToken) -> Result<WaveEditReport, ScoreError> {
        let (s, e, chans) = self.selection(sel)?;
        let n = e.saturating_sub(s) as u64;
        if points.is_empty() || points.len() > MAX_WAVE_ENVELOPE_POINTS {
            return Err(ScoreError::OutOfRange);
        }
        for (i, (offset, db)) in points.iter().enumerate() {
            check_db(*db, WAVE_ENVELOPE_DB)?;
            if *offset > n || (i > 0 && points[i.saturating_sub(1)].0 >= *offset) {
                return Err(ScoreError::OutOfRange);
            }
        }
        let gain = |k: u64| -> f64 {
            let i = points.partition_point(|(o, _)| *o <= k);
            let db = match (i.checked_sub(1).and_then(|j| points.get(j)), points.get(i)) {
                (None, Some((_, d))) | (Some((_, d)), None) => *d,
                (Some((o0, d0)), Some((o1, d1))) => {
                    let t = k.saturating_sub(*o0) as f64 / o1.saturating_sub(*o0) as f64;
                    d0 + t * (d1 - d0)
                }
                (None, None) => 0.0,
            };
            db_to_linear(db)
        };
        let max_gain = db_to_linear(points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max));
        for c in &chans {
            let p = peak(&self.channels[*c][s..e], cancel)?;
            if !((f64::from(p) * max_gain) as f32).is_finite() {
                return Err(ScoreError::NotFinite);
            }
        }
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        for c in chans {
            for (k, v) in self.channels[c][s..e].iter_mut().enumerate() {
                *v = (f64::from(*v) * gain(k as u64)) as f32;
            }
        }
        Ok(self.same_length(sel, Vec::new()))
    }

    fn scale(&mut self, sel: WaveSelection, chans: &[usize], gains: &[Option<f64>], cancel: &CancellationToken) -> Result<WaveEditReport, ScoreError> {
        let (s, e) = self.range(sel.start, sel.end)?;
        for (c, g) in chans.iter().zip(gains) {
            if let Some(g) = g {
                let p = peak(&self.channels[*c][s..e], cancel)?;
                if !((f64::from(p) * db_to_linear(*g)) as f32).is_finite() {
                    return Err(ScoreError::NotFinite);
                }
            }
        }
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        for (c, g) in chans.iter().zip(gains) {
            if let Some(g) = g {
                let lin = db_to_linear(*g);
                for v in &mut self.channels[*c][s..e] {
                    *v = (f64::from(*v) * lin) as f32;
                }
            }
        }
        Ok(self.same_length(sel, gains.to_vec()))
    }

    /// Multiplies the selection by `10^(db/20)` (`db` in [`WAVE_AMPLIFY_DB`]).
    pub fn amplify(&mut self, sel: WaveSelection, db: f64, cancel: &CancellationToken) -> Result<WaveEditReport, ScoreError> {
        let (_, _, chans) = self.selection(sel)?;
        check_db(db, WAVE_AMPLIFY_DB)?;
        let gains = vec![Some(db); chans.len()];
        self.scale(sel, &chans, &gains, cancel)
    }

    /// Scales the selection so its sample peak reads `target_dbfs` (in [`WAVE_NORMALIZE_DBFS`]):
    /// `linked` uses one gain from the largest masked-channel peak, otherwise one gain per channel.
    pub fn normalize(&mut self, sel: WaveSelection, target_dbfs: f64, linked: bool, cancel: &CancellationToken) -> Result<WaveEditReport, ScoreError> {
        let (s, e, chans) = self.selection(sel)?;
        check_db(target_dbfs, WAVE_NORMALIZE_DBFS)?;
        let mut peaks = Vec::with_capacity(chans.len());
        for c in &chans {
            peaks.push(f64::from(peak(&self.channels[*c][s..e], cancel)?));
        }
        let to_gain = |p: f64| (p > 0.0).then(|| target_dbfs - 20.0 * p.log10());
        let gains: Vec<Option<f64>> = if linked {
            let p = peaks.iter().copied().fold(0.0, f64::max);
            vec![to_gain(p); chans.len()]
        } else {
            peaks.iter().map(|p| to_gain(*p)).collect()
        };
        self.scale(sel, &chans, &gains, cancel)
    }

    /// Runs a same-length channel process `f(input, out)` (for example a spectral edit) on every
    /// masked channel into scratch, and replaces the channels only when every call succeeded and every
    /// output sample is finite (else the first error, or `NotFinite`; nothing changes).
    pub fn process_channels(
        &mut self,
        mask: u32,
        cancel: &CancellationToken,
        mut f: impl FnMut(&[f32], &mut [f32]) -> Result<(), ScoreError>,
    ) -> Result<WaveEditReport, ScoreError> {
        let chans = self.mask_channels(mask)?;
        let mut done = Vec::with_capacity(chans.len());
        for c in &chans {
            cancel.check().map_err(|_| ScoreError::Canceled)?;
            let input = &self.channels[*c];
            let mut out = Vec::new();
            out.try_reserve_exact(input.len()).map_err(|_| ScoreError::CapacityExceeded)?;
            out.resize(input.len(), 0.0);
            f(input, &mut out)?;
            if out.iter().any(|v| !v.is_finite()) {
                return Err(ScoreError::NotFinite);
            }
            done.push(out);
        }
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        for (c, out) in chans.iter().zip(done) {
            self.channels[*c] = out;
        }
        Ok(WaveEditReport { frames_before: self.frames(), frames_after: self.frames(), changed_start: 0, changed_end: self.frames(), gains_db: Vec::new() })
    }

    /// Negates the selection (exact).
    pub fn invert(&mut self, sel: WaveSelection, cancel: &CancellationToken) -> Result<WaveEditReport, ScoreError> {
        let (s, e, chans) = self.selection(sel)?;
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        for c in chans {
            for v in &mut self.channels[c][s..e] {
                *v = -*v;
            }
        }
        Ok(self.same_length(sel, Vec::new()))
    }

    /// Reverses the selection in time (exact).
    pub fn reverse(&mut self, sel: WaveSelection, cancel: &CancellationToken) -> Result<WaveEditReport, ScoreError> {
        let (s, e, chans) = self.selection(sel)?;
        cancel.check().map_err(|_| ScoreError::Canceled)?;
        for c in chans {
            self.channels[c][s..e].reverse();
        }
        Ok(self.same_length(sel, Vec::new()))
    }
}
