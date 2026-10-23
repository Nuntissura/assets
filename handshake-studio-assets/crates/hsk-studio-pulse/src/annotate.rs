//! Markers, transitions and the work area as edits of Folio's stored records (lane
//! SMB-PULSE-PLAYBACK: Folio's `StudioMarker`, `TimelineTransition` and `WorkArea` are the model;
//! pulse keeps no parallel record set). Every [`RecordEdit`] is one proposal through the same path
//! as the clip edits: one revision step, one Folio validation, one inverse
//! ([`crate::plan_timeline_revert`] is its undo and redo).
//!
//! Scopes (Studio readings recorded in folio `timeline.rs`): a SEQUENCE marker sits in sequence
//! ticks on whole frames and never moves with clips; a CLIP marker sits in the clip's SOURCE ticks,
//! so it travels with the clip, a trim or slip only hides it, and a clip cut in two (razor,
//! insert, overwrite, range lift/extract) hands each marker to the piece whose source window holds
//! its start (`split_records`). Markers of a removed clip go with it and are listed in
//! [`crate::EditProposal::removed_markers`]; transitions that no longer play after an edit are
//! removed and listed in [`crate::EditProposal::dropped_transitions`].
#![deny(clippy::arithmetic_side_effects)]

use crate::error::{PulseError, Result};
use crate::range::{Grid, TickDelta, TickRange};
use crate::timeline_edit::{EditContext, EditProposal, Footprint, propose, rate_of};
use crate::transition::{check_at, locate};
use hsk_studio_accord::{CancellationToken, Ticks};
pub use hsk_studio_folio::{LabelColor, MarkerRole, StudioMarker, WorkArea};
use hsk_studio_folio::{MAX_MARKERS_PER_SEQUENCE, Snapshot, StudioClip, StudioSequence};

const CANCEL_STRIDE: usize = 256;

/// Where a new marker goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkerScope {
    /// Sequence time; does not move with clips.
    Sequence,
    /// The clip's SOURCE time; travels with the clip.
    Clip { clip_id: String },
}

/// Typed record commands (STU-VID-032a apply transition, STU-VID-034 markers, STU-VID-035 work
/// area). Each is one undoable proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordEdit {
    AddMarker {
        scope: MarkerScope,
        marker: StudioMarker,
    },
    /// Replaces the marker with the same id where it is (name, comment, colour, span, role).
    ReplaceMarker {
        marker: StudioMarker,
    },
    RemoveMarker {
        marker_id: String,
    },
    /// Sets (`Some`) or clears (`None`) the transition at the cut after `outgoing_clip_id`.
    SetTransition {
        outgoing_clip_id: String,
        transition: Option<TimelineTransitionBox>,
    },
    SetWorkArea(Option<WorkArea>),
}

/// Boxed transition record (keeps [`RecordEdit`] small).
pub type TimelineTransitionBox = Box<hsk_studio_folio::TimelineTransition>;

fn marker_count(sequence: &StudioSequence) -> usize {
    sequence
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .fold(sequence.markers.len(), |n, c| {
            n.saturating_add(c.markers.len())
        })
}

/// The marker `marker_id`: `None` = sequence scope, `Some((t, c))` = on that clip.
fn find_marker(
    sequence: &StudioSequence,
    marker_id: &str,
) -> Option<(Option<(usize, usize)>, usize)> {
    if let Some(i) = sequence
        .markers
        .iter()
        .position(|m| m.marker_id == marker_id)
    {
        return Some((None, i));
    }
    sequence.tracks.iter().enumerate().find_map(|(t, track)| {
        track.clips.iter().enumerate().find_map(|(c, clip)| {
            clip.markers
                .iter()
                .position(|m| m.marker_id == marker_id)
                .map(|i| (Some((t, c)), i))
        })
    })
}

fn sequence_marker_grid(sequence: &StudioSequence, marker: &StudioMarker) -> Result<()> {
    let grid = Grid::Frames(rate_of(sequence)?);
    grid.check_position(marker.start)?;
    grid.check_delta(TickDelta::between(Ticks::new(0), marker.duration))
}

fn apply_record(
    seq: &StudioSequence,
    edit: &RecordEdit,
    ctx: &EditContext,
    reads: &mut Footprint,
) -> Result<StudioSequence> {
    let mut out = seq.clone();
    match edit {
        RecordEdit::AddMarker { scope, marker } => {
            if marker_count(seq) >= MAX_MARKERS_PER_SEQUENCE {
                return Err(PulseError::LimitExceeded);
            }
            marker.start.checked_add(marker.duration)?;
            if find_marker(seq, &marker.marker_id).is_some() {
                return Err(PulseError::DuplicateId);
            }
            match scope {
                MarkerScope::Sequence => {
                    sequence_marker_grid(seq, marker)?;
                    out.markers.push(marker.clone());
                }
                MarkerScope::Clip { clip_id } => {
                    let (t, c) = locate(seq, clip_id).ok_or(PulseError::UnknownClip)?;
                    reads.add(clip_id);
                    out.tracks[t].clips[c].markers.push(marker.clone());
                }
            }
        }
        RecordEdit::ReplaceMarker { marker } => {
            marker.start.checked_add(marker.duration)?;
            let (place, i) =
                find_marker(seq, &marker.marker_id).ok_or(PulseError::UnknownMarker)?;
            match place {
                None => {
                    sequence_marker_grid(seq, marker)?;
                    out.markers[i] = marker.clone();
                }
                Some((t, c)) => out.tracks[t].clips[c].markers[i] = marker.clone(),
            }
        }
        RecordEdit::RemoveMarker { marker_id } => {
            let (place, i) = find_marker(seq, marker_id).ok_or(PulseError::UnknownMarker)?;
            match place {
                None => {
                    out.markers.remove(i);
                }
                Some((t, c)) => {
                    out.tracks[t].clips[c].markers.remove(i);
                }
            }
        }
        RecordEdit::SetTransition {
            outgoing_clip_id,
            transition,
        } => {
            let (t, c) = locate(seq, outgoing_clip_id).ok_or(PulseError::UnknownClip)?;
            reads.add(outgoing_clip_id);
            if let Some(tr) = transition {
                let taken = seq.tracks.iter().flat_map(|k| k.clips.iter()).any(|k| {
                    [&k.transitions.head, &k.transitions.tail]
                        .into_iter()
                        .flatten()
                        .any(|x| x.transition_id == tr.transition_id)
                        && k.clip_id != *outgoing_clip_id
                });
                if taken {
                    return Err(PulseError::DuplicateId);
                }
            }
            out.tracks[t].clips[c].transitions.tail = transition.as_deref().cloned();
            if transition.is_some() {
                check_at(&out, t, c, ctx.extents)?;
                // No other transition on the track may overlap the new one.
                let mine = crate::transition::placement_at(&out, t, c)?;
                for k in 0..out.tracks[t].clips.len() {
                    if k != c
                        && out.tracks[t].clips[k].transitions.tail.is_some()
                        && let Ok(other) = crate::transition::placement_at(&out, t, k)
                        && other.span.overlaps(mine.span)
                    {
                        return Err(PulseError::Overlap);
                    }
                }
            }
        }
        RecordEdit::SetWorkArea(area) => {
            if let Some(a) = area {
                if a.start >= a.end {
                    return Err(PulseError::InvalidRange);
                }
                let grid = Grid::Frames(rate_of(seq)?);
                grid.check_position(a.start)?;
                grid.check_position(a.end)?;
            }
            out.work_area = *area;
        }
    }
    Ok(out)
}

/// Plans one record command against `snapshot` at exactly `expected_revision`.
pub fn plan_record_edit(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    edit: &RecordEdit,
    ctx: &EditContext,
) -> Result<EditProposal> {
    propose(
        snapshot,
        sequence_id,
        expected_revision,
        ctx,
        true,
        |old, reads| apply_record(old, edit, ctx, reads),
    )
}

/// Hands the markers of a clip cut in two to the piece whose source window holds their start;
/// the left piece keeps the head transition, the right piece the tail transition.
pub(crate) fn split_records(first: &mut StudioClip, second: &mut StudioClip) {
    let reversed = first.speed.is_reversed();
    let (to_second, to_first): (Vec<StudioMarker>, Vec<StudioMarker>) =
        std::mem::take(&mut first.markers)
            .into_iter()
            .partition(|m| {
                if reversed {
                    m.start < second.source_out
                } else {
                    m.start >= second.source_in
                }
            });
    first.markers = to_first;
    second.markers = to_second;
    first.transitions.tail = None;
    second.transitions.head = None;
}

/// Timeline position of `[a, b)` source ticks of `clip` (clipped to its playing window); `None`
/// when no part of it plays. Positions between ticks round down.
fn source_to_timeline(clip: &StudioClip, a: Ticks, b: Ticks) -> Result<Option<TickRange>> {
    let visible = if a == b {
        clip.source_in <= a && a < clip.source_out
    } else {
        a < clip.source_out && b > clip.source_in
    };
    if !visible {
        return Ok(None);
    }
    let lo = a.max(clip.source_in);
    let hi = b.min(clip.source_out).max(lo);
    let to_timeline = |offset: u64| -> Result<u64> {
        let num = u128::from(clip.speed.numerator.unsigned_abs());
        let scaled = u128::from(offset)
            .checked_mul(u128::from(clip.speed.denominator))
            .ok_or(PulseError::Overflow)?;
        u64::try_from(scaled.checked_div(num).ok_or(PulseError::ZeroSpeed)?)
            .map_err(|_| PulseError::Overflow)
    };
    let (from, to) = if clip.speed.is_reversed() {
        (
            clip.source_out.checked_sub(hi)?.value(),
            clip.source_out.checked_sub(lo)?.value(),
        )
    } else {
        (
            lo.checked_sub(clip.source_in)?.value(),
            hi.checked_sub(clip.source_in)?.value(),
        )
    };
    let start = clip
        .timeline_start
        .checked_add(Ticks::new(to_timeline(from)?))?;
    let end = clip
        .timeline_start
        .checked_add(Ticks::new(to_timeline(to)?))?;
    TickRange::new(start, end).map(Some)
}

/// Where a marker sits in sequence time: `clip = None` for a sequence marker; `None` result = a
/// clip marker outside the clip's playing window.
pub fn marker_span(clip: Option<&StudioClip>, marker: &StudioMarker) -> Result<Option<TickRange>> {
    match clip {
        None => TickRange::from_start_duration(marker.start, marker.duration).map(Some),
        Some(clip) => source_to_timeline(
            clip,
            marker.start,
            marker.start.checked_add(marker.duration)?,
        ),
    }
}

/// Every visible marker of the sequence as `(marker_id, sequence span)`, ordered by start then id.
pub fn marker_spans(
    sequence: &StudioSequence,
    cancel: &CancellationToken,
) -> Result<Vec<(String, TickRange)>> {
    let mut out = Vec::with_capacity(marker_count(sequence));
    for marker in &sequence.markers {
        if let Some(span) = marker_span(None, marker)? {
            out.push((marker.marker_id.clone(), span));
        }
    }
    for (i, clip) in sequence
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .enumerate()
    {
        if i.is_multiple_of(CANCEL_STRIDE) {
            cancel.check()?;
        }
        for marker in &clip.markers {
            if let Some(span) = marker_span(Some(clip), marker)? {
                out.push((marker.marker_id.clone(), span));
            }
        }
    }
    out.sort_by(|a, b| (a.1.start(), &a.0).cmp(&(b.1.start(), &b.0)));
    Ok(out)
}

/// Playhead navigation (STU-VID-035): the first marker start strictly after `after`.
pub fn next_marker(
    sequence: &StudioSequence,
    after: Ticks,
    cancel: &CancellationToken,
) -> Result<Option<Ticks>> {
    Ok(marker_spans(sequence, cancel)?
        .into_iter()
        .map(|(_, span)| span.start())
        .find(|start| *start > after))
}

/// Playhead navigation (STU-VID-035): the last marker start strictly before `before`.
pub fn prev_marker(
    sequence: &StudioSequence,
    before: Ticks,
    cancel: &CancellationToken,
) -> Result<Option<Ticks>> {
    Ok(marker_spans(sequence, cancel)?
        .into_iter()
        .map(|(_, span)| span.start())
        .rfind(|start| *start < before))
}

/// The span playback and export scope to: the work area when set, else `[0, duration)`.
pub fn play_range(sequence: &StudioSequence) -> Result<TickRange> {
    match sequence.work_area {
        Some(a) => TickRange::new(a.start, a.end),
        None => TickRange::new(Ticks::new(0), sequence.duration),
    }
}
