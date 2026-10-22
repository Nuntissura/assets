//! Snapping targets (STU-VID-035: "Snapping is a toggleable magnet that attracts dragged edges and
//! the playhead to edit points, markers, the work-area bounds and the sequence start"; lane
//! SMB-TIMELINE-UI-1, parity row PR-12). Pulse computes WHERE a dragged edge or the playhead may
//! snap; the toggle, the snap distance (a screen distance turned into ticks by the caller) and the
//! drag itself belong to the timeline surface. Nothing here edits a sequence.
//!
//! Targets: the sequence start (tick 0), every clip's timeline start and end (edit points), every
//! visible marker's start and, for span markers, its end (sequence markers in sequence ticks, clip
//! markers mapped from SOURCE ticks through [`crate::marker_span`]), the work-area bounds when set,
//! and the playhead when given. Clips named in `exclude_clips` (the dragged clip and its link-group
//! members) contribute neither edit points nor clip markers: a dragged edge never snaps to itself.
//! Frame-grid snapping ([`crate::snap_to_frame`]) is a different operation and stays separate.
//!
//! Bounds (V4-01/02): clip and marker counts are checked against Folio's per-sequence limits before
//! any target is collected (`limit_exceeded`), every tick sum is checked (`overflow`), the cancel
//! token is polled every 256 clips. Premiere Pro's snap priorities and default snap distance are
//! UNVERIFIED (D5): ties resolve deterministically (nearer first, then earlier tick, then
//! [`SnapKind`] order).
#![deny(clippy::arithmetic_side_effects)]

use crate::annotate::marker_span;
use crate::error::{PulseError, Result};
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{MAX_CLIPS_PER_SEQUENCE, MAX_MARKERS_PER_SEQUENCE, StudioSequence};

const CANCEL_STRIDE: usize = 256;

/// What a snap target is (declaration order is the tie-break order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SnapKind {
    /// Tick 0.
    SequenceStart,
    /// A clip's timeline start or end.
    EditPoint,
    /// A marker's start or (span marker) end.
    Marker,
    /// A work-area bound.
    WorkArea,
    /// The playhead.
    Playhead,
}

impl SnapKind {
    /// Stable key for diagnostics and model output.
    pub const fn key(self) -> &'static str {
        match self {
            Self::SequenceStart => "sequence_start",
            Self::EditPoint => "edit_point",
            Self::Marker => "marker",
            Self::WorkArea => "work_area",
            Self::Playhead => "playhead",
        }
    }
}

/// One place a dragged edge or the playhead may snap to. `owner` names the clip (edit points) or
/// the marker (markers) it came from; `None` for the sequence start, work area and playhead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapTarget {
    pub at: Ticks,
    pub kind: SnapKind,
    pub owner: Option<String>,
}

fn target(at: Ticks, kind: SnapKind, owner: Option<&str>) -> SnapTarget {
    SnapTarget {
        at,
        kind,
        owner: owner.map(str::to_owned),
    }
}

/// Every snap target of `sequence` (module docs), sorted by tick then kind; targets with the same
/// tick and kind are kept once (the first owner in track order). `limit_exceeded` when the clip
/// count passes `MAX_CLIPS_PER_SEQUENCE` or the marker count passes `MAX_MARKERS_PER_SEQUENCE`;
/// `overflow` when a clip end or marker end passes `u64` ticks.
pub fn snap_targets(
    sequence: &StudioSequence,
    playhead: Option<Ticks>,
    exclude_clips: &[&str],
    cancel: &CancellationToken,
) -> Result<Vec<SnapTarget>> {
    cancel.check()?;
    let clips = sequence
        .tracks
        .iter()
        .fold(0usize, |n, t| n.saturating_add(t.clips.len()));
    if clips > MAX_CLIPS_PER_SEQUENCE {
        return Err(PulseError::LimitExceeded);
    }
    let markers = sequence
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .fold(sequence.markers.len(), |n, c| n.saturating_add(c.markers.len()));
    if markers > MAX_MARKERS_PER_SEQUENCE {
        return Err(PulseError::LimitExceeded);
    }
    // Upper bound: start, two per clip, two per marker, two work-area bounds, playhead.
    let capacity = clips
        .saturating_mul(2)
        .saturating_add(markers.saturating_mul(2))
        .saturating_add(4);
    let mut out = Vec::with_capacity(capacity);
    out.push(target(Ticks::new(0), SnapKind::SequenceStart, None));
    let push_span = |out: &mut Vec<SnapTarget>, start: Ticks, end: Ticks, id: &str| {
        out.push(target(start, SnapKind::Marker, Some(id)));
        if end != start {
            out.push(target(end, SnapKind::Marker, Some(id)));
        }
    };
    for marker in &sequence.markers {
        if let Some(span) = marker_span(None, marker)? {
            push_span(&mut out, span.start(), span.end(), &marker.marker_id);
        }
    }
    let mut seen = 0usize;
    for track in &sequence.tracks {
        for clip in &track.clips {
            if seen.is_multiple_of(CANCEL_STRIDE) {
                cancel.check()?;
            }
            seen = seen.saturating_add(1);
            if exclude_clips.contains(&clip.clip_id.as_str()) {
                continue;
            }
            let end = clip.timeline_end().ok_or(PulseError::Overflow)?;
            out.push(target(clip.timeline_start, SnapKind::EditPoint, Some(&clip.clip_id)));
            out.push(target(end, SnapKind::EditPoint, Some(&clip.clip_id)));
            for marker in &clip.markers {
                if let Some(span) = marker_span(Some(clip), marker)? {
                    push_span(&mut out, span.start(), span.end(), &marker.marker_id);
                }
            }
        }
    }
    if let Some(area) = &sequence.work_area {
        out.push(target(area.start, SnapKind::WorkArea, None));
        out.push(target(area.end, SnapKind::WorkArea, None));
    }
    if let Some(p) = playhead {
        out.push(target(p, SnapKind::Playhead, None));
    }
    cancel.check()?;
    // Stable: equal (tick, kind) pairs keep track order, so dedup keeps the first owner.
    out.sort_by_key(|t| (t.at, t.kind));
    out.dedup_by(|b, a| a.at == b.at && a.kind == b.kind);
    Ok(out)
}

/// The target nearest to `at` within `distance` ticks (inclusive), over `targets` sorted as
/// [`snap_targets`] returns them; ties resolve nearer first, then earlier tick, then [`SnapKind`]
/// order. `None` when no target is that close.
pub fn nearest_snap(targets: &[SnapTarget], at: Ticks, distance: Ticks) -> Option<&SnapTarget> {
    let lo = at.value().saturating_sub(distance.value());
    let hi = at.value().saturating_add(distance.value());
    let first = targets.partition_point(|t| t.at.value() < lo);
    targets
        .get(first..)?
        .iter()
        .take_while(|t| t.at.value() <= hi)
        .min_by_key(|t| (t.at.value().abs_diff(at.value()), t.at, t.kind))
}

/// The playhead's next or previous edit point (STU-VID-035 "to the next or previous edit on any
/// track"): the first edit point strictly after (`forward`) or the last strictly before `from`,
/// over `targets` sorted as [`snap_targets`] returns them.
pub fn adjacent_edit(targets: &[SnapTarget], from: Ticks, forward: bool) -> Option<Ticks> {
    let edits = targets
        .iter()
        .filter(|t| matches!(t.kind, SnapKind::EditPoint | SnapKind::SequenceStart));
    if forward {
        edits.map(|t| t.at).find(|t| *t > from)
    } else {
        edits.map(|t| t.at).rfind(|t| *t < from)
    }
}
