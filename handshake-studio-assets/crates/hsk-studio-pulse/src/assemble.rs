//! Three- and four-point assembly (STU-VID-024) with source patching (STU-VID-025): an incoming
//! source window is placed on the patched destination tracks either by INSERT (content at or after
//! the sequence in point shifts later by the clip's duration on the patched tracks and on every
//! other sync-locked track; a clip straddling the in point is split first) or by OVERWRITE (the
//! clip replaces whatever lies under its own duration on the patched tracks; nothing moves).
//!
//! The fourth point is derived from three; four disagreeing points need an explicit
//! [`FourPointFit`] (`fit_required` otherwise, never a silent choice). The whole multi-track edit
//! is ONE proposal (one revision step, one Folio validation, one inverse; STU-VID-040).
//! Premiere Pro behaviour this mirrors (straddler split on sync-locked tracks, overwrite of a
//! partly covered clip) is UNVERIFIED (D5 oracle session).
#![deny(clippy::arithmetic_side_effects)]

use crate::error::{PulseError, Result};
use crate::range::{TickDelta, TickRange, lift_span};
use crate::rational::{SpeedMath, reduced_speed};
use crate::timeline_edit::{
    EditContext, EditProposal, Footprint, check_extent, grid_of, lane_of, propose, rate_of,
    split_clip, write_pieces,
};
use hsk_studio_accord::Ticks;
use hsk_studio_folio::{
    ClipSchema, ClipSource, MAX_CLIPS_PER_SEQUENCE, MAX_TRACKS_PER_SEQUENCE, Snapshot, Speed,
    StudioClip, StudioSequence,
};
use std::collections::BTreeSet;

/// Up to four edit points; exactly the missing one is derived (STU-VID-024).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AssemblyPoints {
    pub source_in: Option<Ticks>,
    pub source_out: Option<Ticks>,
    pub sequence_in: Option<Ticks>,
    pub sequence_out: Option<Ticks>,
}

/// The declared choices when four points disagree (STU-VID-024).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FourPointFit {
    /// Keep both windows; the clip speed becomes source span / sequence span (sign kept).
    ChangeSpeed,
    /// Keep source out and the sequence span; source in moves.
    TrimHead,
    /// Keep source in and the sequence span; source out moves.
    TrimTail,
    /// Keep the source window; sequence out is ignored.
    IgnoreSequenceOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssemblyMode {
    Insert,
    Overwrite,
}

/// Source patching: one incoming stream lands on `track_id` as a new clip `clip_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePatch {
    pub track_id: String,
    pub clip_id: String,
}

/// One insert or overwrite edit. Pulse mints no ids: new clip ids come from `patches`, the right
/// halves of split clips take `fresh_clip_ids` in edit order (insert: track order; overwrite:
/// patch order), and `link_group` (when given, a
/// group id not yet in use) joins the new clips.
#[derive(Clone, Debug, PartialEq)]
pub struct AssemblyEdit {
    pub mode: AssemblyMode,
    pub source: ClipSource,
    pub speed: Speed,
    pub points: AssemblyPoints,
    pub fit: Option<FourPointFit>,
    pub patches: Vec<SourcePatch>,
    pub link_group: Option<String>,
    pub fresh_clip_ids: Vec<String>,
}

/// The four points after derivation (sequence out = `sequence_in + duration`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedPoints {
    pub source_in: Ticks,
    pub source_out: Ticks,
    pub sequence_in: Ticks,
    pub duration: Ticks,
    pub speed: Speed,
}

fn span_between(start: Ticks, end: Ticks) -> Result<Ticks> {
    if end <= start {
        return Err(PulseError::InvalidRange);
    }
    end.checked_sub(start).map_err(PulseError::from)
}

fn source_before(end: Ticks, span: Ticks) -> Result<Ticks> {
    end.checked_sub(span)
        .map_err(|_| PulseError::PastSourceStart)
}

/// Derives the missing point (STU-VID-024): needs at least three points; four points that
/// disagree need `fit`. Every value is checked; nothing rounds.
pub fn resolve_points(
    points: AssemblyPoints,
    speed: Speed,
    fit: Option<FourPointFit>,
) -> Result<ResolvedPoints> {
    reduced_speed(speed.numerator, speed.denominator)
        .ok()
        .filter(|r| *r == speed)
        .ok_or(PulseError::InvalidSpeed)?;
    let resolved = |source_in, source_out, sequence_in, speed: Speed| -> Result<ResolvedPoints> {
        let duration = speed.duration_for_span(span_between(source_in, source_out)?)?;
        Ok(ResolvedPoints {
            source_in,
            source_out,
            sequence_in,
            duration,
            speed,
        })
    };
    match (
        points.source_in,
        points.source_out,
        points.sequence_in,
        points.sequence_out,
    ) {
        (Some(si), Some(so), Some(qi), None) => resolved(si, so, qi, speed),
        (Some(si), Some(so), None, Some(qo)) => {
            let duration = speed.duration_for_span(span_between(si, so)?)?;
            let qi = qo
                .checked_sub(duration)
                .map_err(|_| PulseError::Underflow)?;
            resolved(si, so, qi, speed)
        }
        (Some(si), None, Some(qi), Some(qo)) => {
            let so = si.checked_add(speed.span_for_duration(span_between(qi, qo)?)?)?;
            resolved(si, so, qi, speed)
        }
        (None, Some(so), Some(qi), Some(qo)) => {
            let si = source_before(so, speed.span_for_duration(span_between(qi, qo)?)?)?;
            resolved(si, so, qi, speed)
        }
        (Some(si), Some(so), Some(qi), Some(qo)) => {
            let target = span_between(qi, qo)?;
            let span = span_between(si, so)?;
            if speed.duration_for_span(span).ok() == Some(target) {
                return resolved(si, so, qi, speed);
            }
            match fit.ok_or(PulseError::FitRequired)? {
                FourPointFit::IgnoreSequenceOut => resolved(si, so, qi, speed),
                FourPointFit::TrimTail => {
                    let so = si.checked_add(speed.span_for_duration(target)?)?;
                    resolved(si, so, qi, speed)
                }
                FourPointFit::TrimHead => {
                    let si = source_before(so, speed.span_for_duration(target)?)?;
                    resolved(si, so, qi, speed)
                }
                FourPointFit::ChangeSpeed => {
                    let magnitude =
                        i64::try_from(span.value()).map_err(|_| PulseError::InvalidSpeed)?;
                    let signed = if speed.is_reversed() {
                        magnitude.checked_neg().ok_or(PulseError::InvalidSpeed)?
                    } else {
                        magnitude
                    };
                    resolved(si, so, qi, reduced_speed(signed, target.value())?)
                }
            }
        }
        _ => Err(PulseError::InvalidInput),
    }
}

/// Plans one insert or overwrite against `snapshot` at exactly `expected_revision` (one revision
/// step; the proposal's inverse undoes the whole multi-track edit).
pub fn plan_assembly_edit(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    edit: &AssemblyEdit,
    ctx: &EditContext,
) -> Result<EditProposal> {
    // Hostile sizes are refused before anything is cloned or allocated.
    if edit.patches.is_empty() {
        return Err(PulseError::InvalidInput);
    }
    if edit.patches.len() > MAX_TRACKS_PER_SEQUENCE
        || edit.fresh_clip_ids.len() > MAX_CLIPS_PER_SEQUENCE
    {
        return Err(PulseError::LimitExceeded);
    }
    let points = resolve_points(edit.points, edit.speed, edit.fit)?;
    propose(
        snapshot,
        sequence_id,
        expected_revision,
        ctx,
        |old, reads| assemble(old, edit, points, ctx, reads),
    )
}

fn assemble(
    seq: &StudioSequence,
    edit: &AssemblyEdit,
    points: ResolvedPoints,
    ctx: &EditContext,
    reads: &mut Footprint,
) -> Result<StudioSequence> {
    let rate = rate_of(seq)?;
    let existing = seq
        .tracks
        .iter()
        .fold(0usize, |n, t| n.saturating_add(t.clips.len()));
    // Every patch adds one clip and every split one more; refuse before cloning the sequence.
    let worst = existing
        .saturating_add(edit.patches.len())
        .saturating_add(edit.fresh_clip_ids.len().min(seq.tracks.len()));
    if worst > MAX_CLIPS_PER_SEQUENCE {
        return Err(PulseError::LimitExceeded);
    }
    let span = TickRange::from_start_duration(points.sequence_in, points.duration)?;
    let mut taken: BTreeSet<String> = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|c| c.clip_id.clone())
        .collect();
    if let Some(group) = &edit.link_group {
        let in_use = seq
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .any(|c| c.link_group.as_ref() == Some(group));
        if in_use {
            return Err(PulseError::DuplicateId);
        }
    }
    let mut patched = Vec::with_capacity(edit.patches.len());
    for patch in &edit.patches {
        ctx.cancel.check()?;
        let t = seq
            .tracks
            .iter()
            .position(|t| t.track_id == patch.track_id)
            .ok_or(PulseError::UnknownTrack)?;
        if patched.iter().any(|(p, _)| *p == t) {
            // Two streams on one track would overlap: the edit is beyond that track's capacity.
            return Err(PulseError::Overlap);
        }
        let track = &seq.tracks[t];
        reads.add(&track.track_id);
        if track.locked {
            return Err(PulseError::TrackLocked);
        }
        let grid = grid_of(track, rate);
        grid.check_position(points.sequence_in)?;
        grid.check_delta(TickDelta::between(Ticks::new(0), points.duration))?;
        if !taken.insert(patch.clip_id.clone()) {
            return Err(PulseError::DuplicateId);
        }
        patched.push((t, patch.clip_id.as_str()));
    }
    let mut incoming = StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: String::new(),
        source_ref: edit.source.clone(),
        source_in: points.source_in,
        source_out: points.source_out,
        timeline_start: points.sequence_in,
        duration: points.duration,
        speed: points.speed,
        enabled: true,
        link_group: edit.link_group.clone(),
    };
    check_extent(&incoming, points.source_out, ctx)?;
    let mut fresh = edit.fresh_clip_ids.iter();
    let mut out = seq.clone();
    match edit.mode {
        AssemblyMode::Overwrite => {
            for &(t, _) in &patched {
                let track = &seq.tracks[t];
                let covered_link = track.clips.iter().any(|c| {
                    c.link_group.is_some()
                        && c.timeline_start < span.end()
                        && c.timeline_end().is_none_or(|end| end > span.start())
                });
                if covered_link {
                    return Err(PulseError::LinkedEditUnsupported);
                }
                let pieces = lift_span(&lane_of(track)?, span, grid_of(track, rate), ctx.cancel)?;
                write_pieces(&mut out.tracks[t], &pieces, &mut fresh, &mut taken, ctx)?;
            }
        }
        AssemblyMode::Insert => {
            for (t, track) in seq.tracks.iter().enumerate() {
                ctx.cancel.check()?;
                let is_patched = patched.iter().any(|(p, _)| *p == t);
                let has_later = track
                    .clips
                    .iter()
                    .any(|c| c.timeline_end().is_none_or(|end| end > points.sequence_in));
                if !is_patched && !(track.sync_locked && has_later) {
                    continue;
                }
                if !is_patched {
                    reads.add(&track.track_id);
                    if track.locked {
                        return Err(PulseError::SyncLockBlocked);
                    }
                    let grid = grid_of(track, rate);
                    grid.check_delta(TickDelta::between(Ticks::new(0), points.duration))
                        .map_err(|_| PulseError::SyncLockBlocked)?;
                }
                insert_gap(
                    &mut out.tracks[t].clips,
                    points,
                    grid_of(track, rate),
                    &mut fresh,
                    &mut taken,
                )?;
            }
        }
    }
    for &(t, clip_id) in &patched {
        incoming.clip_id = clip_id.to_owned();
        let clips = &mut out.tracks[t].clips;
        let at = clips.partition_point(|c| c.timeline_start < points.sequence_in);
        clips.insert(at, incoming.clone());
    }
    Ok(out)
}

/// Opens `[sequence_in, sequence_in + duration)` on one track: a clip straddling the in point is
/// split (its right half takes the next fresh id) and everything at or after it moves later.
fn insert_gap(
    clips: &mut Vec<StudioClip>,
    points: ResolvedPoints,
    grid: crate::range::Grid,
    fresh: &mut std::slice::Iter<'_, String>,
    taken: &mut BTreeSet<String>,
) -> Result<()> {
    let at = points.sequence_in;
    let straddler = clips
        .iter()
        .position(|c| c.timeline_start < at && c.timeline_end().is_none_or(|end| end > at));
    if let Some(s) = straddler {
        if clips[s].link_group.is_some() {
            return Err(PulseError::LinkedEditUnsupported);
        }
        grid.check_position(at)?;
        let id = fresh.next().ok_or(PulseError::InsufficientIds)?;
        if !taken.insert(id.clone()) {
            return Err(PulseError::DuplicateId);
        }
        let (left, right) = split_clip(&clips[s], at, id)?;
        clips[s] = left;
        clips.insert(s.saturating_add(1), right);
    }
    for clip in clips.iter_mut().filter(|c| c.timeline_start >= at) {
        clip.timeline_start = clip.timeline_start.checked_add(points.duration)?;
        clip.timeline_end().ok_or(PulseError::Overflow)?;
    }
    Ok(())
}
