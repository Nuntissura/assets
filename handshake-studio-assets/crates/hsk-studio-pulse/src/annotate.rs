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
//!
//! The third scope (lane SMB-PR-MEDIA-MARKERS-1, STU-VID-034: "lives on a media item and appears on
//! every clip cut from it") is stored beside the document (`StudioMediaMarkerRecords`, committed by the
//! session), in the media item's SOURCE ticks. [`clip_appearing_markers`] and
//! [`sequence_appearing_markers`] answer what appears, each marker labelled with its scope, from records
//! the host passes in: a session has no pulse edge. A clip is cut from a media item when its source is
//! `ClipSource::Media` with that digest, whatever its manifest handle or asset id. A clip with a nested
//! sequence or a primitive as its source has no media item: its own markers appear, no media item's do,
//! and [`ClipMediaItem`] says which case it is; the markers of the clips inside a nested sequence are not
//! lifted to the outer clip.
//!
//! The mapping is the picture's ([`crate::frame_layers`]), read the other way: speed and reverse through
//! `source_to_timeline`, and a frame-held clip plays one source tick at every timeline tick, so a marker
//! holding that tick appears over the whole clip and any other appears nowhere. On a reversed clip a
//! zero-length marker is on a frame, not on a tick: it appears where the picture's position law reaches the
//! frame holding it (`retime` owns that law; `reversed_frame_tick`), which takes a frame grid, as the picture
//! does (test `every_reported_position_shows_the_markers_frame_in_frame_layers`). A span keeps its source
//! ticks through the clip's end, and a whole-frame span covers exactly its frames. [`clip_appearing_markers`]
//! reads the grid its caller states; every read that has a sequence reads the sequence's grid, as
//! `frame_layers` does. A monitor stating another grid for a source (`frame_layers_on_grids`) can show
//! another frame there on a reversed clip: a named gap, F-R57-5 / SD-56
//! (test `a_reversed_point_from_a_source_at_another_rate_is_read_on_the_stated_grid_only_r57_f5`). Held-clip readings, the
//! clause being silent and vendor parity UNVERIFIED (D5): a hold is matched by exact tick, as this read
//! has no frame grid, so a marker inside the held frame but past its first tick is hidden; a held frame
//! outside the window still counts, as playback still shows it. [`marker_span`] and [`marker_spans`],
//! used for playhead navigation, keep their window mapping for a held clip, whose picture has no frame to
//! agree with.
//!
//! A marker that does not appear is hidden, not lost: it stays in its record, comes back when an edit
//! brings its position into play, and is only counted here. A disabled clip's markers appear: it is still
//! a clip cut from the item. The read is bounded by the existing `MAX_MARKERS_PER_SEQUENCE` over what
//! appears (`limit_exceeded`, never a truncated list), maps at most [`MAX_APPEARING_WALK`] markers over a
//! sequence's clips, charged before the first is mapped (`limit_exceeded`, test
//! `a_walk_past_its_budget_is_refused_before_any_marker_is_mapped_r57_f15`), and is cancellable throughout.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::{PulseError, Result};
use crate::range::{Grid, TickDelta, TickRange};
use crate::retime::{SourceFrameGrid, reversed_frame_tick};
use crate::timeline_edit::{EditContext, EditProposal, Footprint, propose, rate_of};
use crate::transition::{check_at, locate};
use hsk_studio_accord::{CancellationToken, Ticks};
pub use hsk_studio_folio::{LabelColor, MarkerRole, StudioMarker, WorkArea};
use hsk_studio_folio::{ClipSource, ContentDigest, MAX_MARKERS_PER_SEQUENCE, Snapshot, StudioClip, StudioMediaMarkerRecords, StudioSequence};
use std::collections::BTreeMap;

const CANCEL_STRIDE: usize = 256;

/// Most markers one sequence's appearing read maps, over every clip: a marker hidden on two clips counts
/// twice, a Studio bound of 256 clips of an item holding the most markers a record set may hold.
pub const MAX_APPEARING_WALK: usize = 1 << 24;

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
/// when no part of it plays. Positions between ticks round down. A point on a reversed clip that holds no
/// frame is read on the grid of `ticks_per_frame` (module docs); zero there is `invalid_input`.
pub(crate) fn source_to_timeline(clip: &StudioClip, a: Ticks, b: Ticks, ticks_per_frame: u64) -> Result<Option<TickRange>> {
    let visible = if a == b {
        clip.source_in <= a && a < clip.source_out
    } else {
        a < clip.source_out && b > clip.source_in
    };
    if !visible {
        return Ok(None);
    }
    if a == b && clip.speed.is_reversed() && clip.retime.frame_hold.is_none() {
        let tick = reversed_frame_tick(clip, a, SourceFrameGrid::new(ticks_per_frame)?)?;
        return TickRange::new(tick, tick).map(Some);
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
/// clip marker outside the clip's playing window. `clip` is read on `sequence`'s frame grid (module docs).
pub fn marker_span(sequence: &StudioSequence, clip: Option<&StudioClip>, marker: &StudioMarker) -> Result<Option<TickRange>> {
    match clip {
        None => TickRange::from_start_duration(marker.start, marker.duration).map(Some),
        Some(clip) => source_to_timeline(
            clip,
            marker.start,
            marker.start.checked_add(marker.duration)?,
            sequence.frame_rate.ticks_per_frame,
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
        if let Some(span) = marker_span(sequence, None, marker)? {
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
            if let Some(span) = marker_span(sequence, Some(clip), marker)? {
                out.push((marker.marker_id.clone(), span));
            }
        }
    }
    out.sort_by(|a, b| (a.1.start(), &a.0).cmp(&(b.1.start(), &b.0)));
    Ok(out)
}

/// Which of the three STU-VID-034 scopes a marker belongs to. The scopes are not the same object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AppearingMarkerScope {
    /// In sequence time; does not move with clips.
    Sequence,
    /// On the clip; travels with it.
    Clip,
    /// On the media item the clip is cut from.
    MediaItem,
}

/// One marker where it appears.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppearingMarker {
    pub scope: AppearingMarkerScope,
    /// The clip it appears on; `None` for a sequence marker. A media item's marker appears once per clip
    /// cut from it, so `clip_id` and `marker.marker_id` together name an appearance.
    pub clip_id: Option<String>,
    /// The stored record, unchanged: its `start` stays in its own time base.
    pub marker: StudioMarker,
    /// Where it appears in sequence time, clipped to what the clip plays.
    pub span: TickRange,
}

/// What a clip is cut from, for the media item scope.
#[derive(Clone, Debug, PartialEq)]
pub enum ClipMediaItem {
    /// `ClipSource::Media`: the item with these bytes.
    Media(ContentDigest),
    /// `ClipSource::Sequence`: a nested sequence has no media item.
    NestedSequence,
    /// `ClipSource::Primitive`: a typed primitive has no media item.
    Primitive,
}
impl ClipMediaItem {
    fn of(clip: &StudioClip) -> Self {
        match &clip.source_ref {
            ClipSource::Media { content_digest, .. } => Self::Media(content_digest.clone()),
            ClipSource::Sequence { .. } => Self::NestedSequence,
            ClipSource::Primitive { .. } => Self::Primitive,
        }
    }
}

/// Every marker that appears on one clip, by timeline position.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipMarkers {
    pub clip_id: String,
    pub media_item: ClipMediaItem,
    pub appearing: Vec<AppearingMarker>,
    /// The clip's own markers that do not appear; still on the clip.
    pub hidden_clip_markers: usize,
    /// Its media item's markers that do not appear on this clip; still on the item.
    pub hidden_media_markers: usize,
}

/// Every marker that appears in one sequence, by timeline position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceMarkers {
    pub appearing: Vec<AppearingMarker>,
    /// Summed over the clips, as [`ClipMarkers::hidden_clip_markers`].
    pub hidden_clip_markers: usize,
    /// Summed over the clips, as [`ClipMarkers::hidden_media_markers`]: a marker hidden on two clips
    /// counts twice.
    pub hidden_media_markers: usize,
    /// Clips with a nested sequence or a primitive as their source.
    pub clips_without_media_item: usize,
}

/// Where `marker` appears on `clip`; `None` when it does not (module docs).
fn appearing_span(clip: &StudioClip, marker: &StudioMarker, ticks_per_frame: u64) -> Result<Option<TickRange>> {
    let end = marker.start.checked_add(marker.duration)?;
    let Some(hold) = &clip.retime.frame_hold else {
        return source_to_timeline(clip, marker.start, end, ticks_per_frame);
    };
    let holds = if marker.duration.value() == 0 {
        marker.start == hold.source_tick
    } else {
        marker.start <= hold.source_tick && hold.source_tick < end
    };
    if !holds {
        return Ok(None);
    }
    TickRange::new(clip.timeline_start, clip.timeline_end().ok_or(PulseError::Overflow)?).map(Some)
}

/// One clip's walk: the clip's markers and `media`, pushed onto `out` under the limit; returns the two
/// hidden counts.
fn walk_clip(
    clip: &StudioClip,
    media: &[StudioMarker],
    ticks_per_frame: u64,
    out: &mut Vec<AppearingMarker>,
    cancel: &CancellationToken,
) -> Result<(usize, usize)> {
    let mut hidden = (0usize, 0usize);
    let own = clip.markers.iter().map(|m| (m, AppearingMarkerScope::Clip));
    for (i, (marker, scope)) in own.chain(media.iter().map(|m| (m, AppearingMarkerScope::MediaItem))).enumerate() {
        if i.is_multiple_of(CANCEL_STRIDE) {
            cancel.check()?;
        }
        match appearing_span(clip, marker, ticks_per_frame)? {
            Some(span) => {
                if out.len() >= MAX_MARKERS_PER_SEQUENCE {
                    return Err(PulseError::LimitExceeded);
                }
                out.push(AppearingMarker { scope, clip_id: Some(clip.clip_id.clone()), marker: marker.clone(), span });
            }
            None if scope == AppearingMarkerScope::Clip => hidden.0 = hidden.0.saturating_add(1),
            None => hidden.1 = hidden.1.saturating_add(1),
        }
    }
    Ok(hidden)
}

/// The markers of the media item `clip` is cut from; `None` when it has no media item.
fn held_by<'a>(clip: &StudioClip, by_digest: &BTreeMap<(&str, &str), &'a [StudioMarker]>) -> Option<&'a [StudioMarker]> {
    match &clip.source_ref {
        ClipSource::Media { content_digest, .. } => {
            Some(by_digest.get(&(content_digest.algorithm.as_str(), content_digest.digest.as_str())).copied().unwrap_or(&[]))
        }
        ClipSource::Sequence { .. } | ClipSource::Primitive { .. } => None,
    }
}

fn sort_appearing(appearing: &mut [AppearingMarker]) {
    appearing.sort_by(|a, b| {
        (a.span.start(), &a.clip_id, a.scope, &a.marker.marker_id).cmp(&(b.span.start(), &b.clip_id, b.scope, &b.marker.marker_id))
    });
}

/// Every marker that appears on `clip`: its own and those of the media item it is cut from, ordered by
/// start, then scope and id. `media` is the parsed record set a session holds (`None` = none stored).
/// `grid` is the source's frame grid, the one the monitor reads it on (module docs).
pub fn clip_appearing_markers(
    clip: &StudioClip,
    media: Option<&StudioMediaMarkerRecords>,
    grid: SourceFrameGrid,
    cancel: &CancellationToken,
) -> Result<ClipMarkers> {
    cancel.check()?;
    let media_item = ClipMediaItem::of(clip);
    let held: &[StudioMarker] = match &media_item {
        ClipMediaItem::Media(digest) => media.and_then(|r| r.entry(digest)).map_or(&[][..], |e| e.markers.as_slice()),
        ClipMediaItem::NestedSequence | ClipMediaItem::Primitive => &[][..],
    };
    let mut appearing = Vec::new();
    let (hidden_clip_markers, hidden_media_markers) = walk_clip(clip, held, grid.ticks_per_frame(), &mut appearing, cancel)?;
    sort_appearing(&mut appearing);
    Ok(ClipMarkers { clip_id: clip.clip_id.clone(), media_item, appearing, hidden_clip_markers, hidden_media_markers })
}

/// Every marker that appears in `sequence`: its sequence markers and, for every clip, what
/// [`clip_appearing_markers`] answers on the sequence's own frame grid, ordered by start, then clip, scope
/// and id.
pub fn sequence_appearing_markers(
    sequence: &StudioSequence,
    media: Option<&StudioMediaMarkerRecords>,
    cancel: &CancellationToken,
) -> Result<SequenceMarkers> {
    cancel.check()?;
    let mut by_digest: BTreeMap<(&str, &str), &[StudioMarker]> = BTreeMap::new();
    for (i, entry) in media.map_or(&[][..], |r| r.media_items.as_slice()).iter().enumerate() {
        if i.is_multiple_of(CANCEL_STRIDE) {
            cancel.check()?;
        }
        let digest = &entry.content_digest;
        by_digest.entry((digest.algorithm.as_str(), digest.digest.as_str())).or_insert(entry.markers.as_slice());
    }
    let mut walk = 0usize;
    for (i, clip) in sequence.tracks.iter().flat_map(|t| t.clips.iter()).enumerate() {
        if i.is_multiple_of(CANCEL_STRIDE) {
            cancel.check()?;
        }
        walk = walk.saturating_add(clip.markers.len()).saturating_add(held_by(clip, &by_digest).map_or(0, <[StudioMarker]>::len));
        if walk > MAX_APPEARING_WALK {
            return Err(PulseError::LimitExceeded);
        }
    }
    let mut out = SequenceMarkers { appearing: Vec::new(), hidden_clip_markers: 0, hidden_media_markers: 0, clips_without_media_item: 0 };
    for marker in &sequence.markers {
        if out.appearing.len() >= MAX_MARKERS_PER_SEQUENCE {
            return Err(PulseError::LimitExceeded);
        }
        let span = TickRange::from_start_duration(marker.start, marker.duration)?;
        out.appearing.push(AppearingMarker { scope: AppearingMarkerScope::Sequence, clip_id: None, marker: marker.clone(), span });
    }
    for clip in sequence.tracks.iter().flat_map(|t| t.clips.iter()) {
        cancel.check()?;
        let held = held_by(clip, &by_digest).unwrap_or_else(|| {
            out.clips_without_media_item = out.clips_without_media_item.saturating_add(1);
            &[]
        });
        let (own, of_media) = walk_clip(clip, held, sequence.frame_rate.ticks_per_frame, &mut out.appearing, cancel)?;
        out.hidden_clip_markers = out.hidden_clip_markers.saturating_add(own);
        out.hidden_media_markers = out.hidden_media_markers.saturating_add(of_media);
    }
    sort_appearing(&mut out.appearing);
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
