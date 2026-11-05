//! Sequence-time edit planners over Folio timeline members (STU-VID-026/027/029). A planner takes
//! a validated Folio `Snapshot`, runs the edit with pulse's tick/lane algebra, and returns a
//! proposal whose prospective document was validated by Folio itself (`validate_document`): pulse
//! does not re-implement document validation, and it never commits (STU-ARC-011).
//!
//! Semantics: a link group (shared non-null `link_group`) is selected and edited together, one
//! clip per track. Trim, slip, lift and extract act on every member. Razor splits every member
//! (caller supplies the new right-half ids and one new link group). Roll and slide act on every
//! member track but only when the link groups of the clips they touch are CLOSED over the edit
//! (every group member is among the touched clips), else `linked_edit_unsupported`. Range lift and
//! extract refuse any linked clip they overlap (a cut would orphan or split a link group).
//! Ripple, extract and close-gap shift later content on the edited tracks and on every other
//! sync-locked track; refused when such a track is locked or the shift would collide. Video-track
//! edits must move by whole frames. Moving a source-out bound later needs a known source length
//! (`SourceExtents`); an unknown length refuses the extension rather than guessing.
use crate::error::{PulseError, Result};
use crate::range::{
    Grid, LanePiece, TickDelta, TickRange, extract_span, gaps, lift_span, offset, ripple_shift,
    ripple_trim_end, ripple_trim_start, roll_edit, slide, trim_end, trim_start,
};
use crate::rational::SpeedMath;
use hsk_studio_accord::{CancellationToken, FrameRate, Ticks};
use hsk_studio_folio::{
    Budget, ClipRetime, ClipSource, ClipTransitions, Code, Diagnostic, Resolver, Snapshot, Speed,
    StudioClip, StudioMarker, StudioSequence, StudioTrack, TimelineTransition, TrackKind, WorkArea,
    validate_document,
};
use std::collections::{BTreeMap, BTreeSet};

/// Typed port: total length of a clip source in SOURCE ticks (the handle bound). `None` = unknown;
/// a source with no end (generated media) returns `Some(Ticks::new(u64::MAX))`.
pub trait SourceExtents {
    fn extent(&self, source: &ClipSource) -> Option<Ticks>;
}

/// Every source length unknown: extensions past the current source out-point are refused.
pub struct UnknownExtents;
impl SourceExtents for UnknownExtents {
    fn extent(&self, _source: &ClipSource) -> Option<Ticks> {
        None
    }
}

/// Caller-owned bounded execution context.
pub struct EditContext<'a> {
    pub budget: Budget,
    pub resolver: &'a dyn Resolver,
    pub extents: &'a dyn SourceExtents,
    pub cancel: &'a CancellationToken,
}

/// Fresh identities for a linked razor: pulse mints no ids. `right_clip_ids` lists
/// `(existing member clip id, new right-half id)` for every member EXCEPT the primary clip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkedRazor {
    pub right_link_group: String,
    pub right_clip_ids: Vec<(String, String)>,
}

/// Typed timeline commands (ids are Folio's string ids; times are ticks).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimelineEdit {
    /// Tail moves so the clip lasts `new_duration`.
    TrimOut {
        clip_id: String,
        new_duration: Ticks,
        ripple: bool,
    },
    /// Head moves to timeline position `new_head`; with `ripple` the clip keeps its start.
    TrimIn {
        clip_id: String,
        new_head: Ticks,
        ripple: bool,
    },
    /// Splits one clip at `at`; the right half gets `new_right_clip_id`. A linked clip needs
    /// `linked` (ids for the other members' right halves and the right halves' new link group).
    Razor {
        clip_id: String,
        at: Ticks,
        new_right_clip_id: String,
        linked: Option<LinkedRazor>,
    },
    /// Moves the edit between a clip and the clip that abuts it on the same track; for a linked
    /// clip every member's edit moves by the same delta.
    Roll {
        left_clip_id: String,
        delta: TickDelta,
    },
    /// Removes the clip (and its link group) and leaves a gap.
    LiftClip { clip_id: String },
    /// Removes the clip (and its link group) and closes the gap on its tracks and on every other
    /// sync-locked track (ripple delete).
    ExtractClip { clip_id: String },
    /// Removes the time span from every targeted track and leaves a gap. A clip straddling the
    /// span becomes two clips; the second takes the next id of `fresh_clip_ids` (track order,
    /// then clip order).
    LiftRange {
        span: TickRange,
        fresh_clip_ids: Vec<String>,
    },
    /// Removes the time span from every targeted track and closes it; other sync-locked tracks
    /// shift by the span.
    ExtractRange {
        span: TickRange,
        fresh_clip_ids: Vec<String>,
    },
    /// Closes the empty span on `track_id` that contains `at`, shifting later content there and on
    /// every other sync-locked track.
    CloseGap { track_id: String, at: Ticks },
    /// Shifts the source window by `delta` SOURCE ticks; timeline start and duration stay.
    Slip { clip_id: String, delta: TickDelta },
    /// Moves the clip by `delta` between its two abutting neighbours.
    Slide { clip_id: String, delta: TickDelta },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipWindow {
    pub source_in: Ticks,
    pub source_out: Ticks,
    pub timeline_start: Ticks,
    pub duration: Ticks,
}

fn window(clip: &StudioClip) -> ClipWindow {
    ClipWindow {
        source_in: clip.source_in,
        source_out: clip.source_out,
        timeline_start: clip.timeline_start,
        duration: clip.duration,
    }
}

/// Which transition slot of a clip a [`ClipChange::ClipTransition`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionSlot {
    Head,
    Tail,
}

/// One difference between the base sequence and the prospective one. The list covers every stored
/// member of the sequence, its tracks and its clips (`diff` destructures all three with no `..`, so
/// a new member does not compile until someone decides how it is reported; test
/// `every_planner_names_every_changed_record`). Sequence-level and track-level entries have no clip:
/// [`ClipChange::clip_id`] answers `None` for them. Each clip's entries come in a fixed order, its
/// [`ClipChange::Retime`] or [`ClipChange::Remove`] first.
#[derive(Clone, Debug, PartialEq)]
pub enum ClipChange {
    Retime {
        clip_id: String,
        track_id: String,
        before: ClipWindow,
        after: ClipWindow,
    },
    Insert {
        track_id: String,
        clip: Box<StudioClip>,
    },
    Remove {
        clip_id: String,
        track_id: String,
        before: ClipWindow,
    },
    /// The clip's `retime` record changed (frame hold, interpolation choice, `Maintain audio pitch`).
    /// A clip whose window also changed has a [`ClipChange::Retime`] first and this entry after it.
    RetimeRecord {
        clip_id: String,
        track_id: String,
        window: ClipWindow,
        before: ClipRetime,
        after: ClipRetime,
    },
    /// The clip's `link_group` changed (merge clips, synchronize, unlink, their undo). A clip that
    /// also moved has a [`ClipChange::Retime`] first and this entry after it.
    Relink {
        clip_id: String,
        track_id: String,
        window: ClipWindow,
        group_before: Option<String>,
        group_after: Option<String>,
    },
    /// The clip's `speed` changed, whether or not its window did (a reverse-only change leaves the
    /// window as it is). A clip whose window also changed has a [`ClipChange::Retime`] first.
    Speed {
        clip_id: String,
        track_id: String,
        window: ClipWindow,
        before: Speed,
        after: Speed,
    },
    /// The clip now sits on another track ([`crate::plan_nudge_track`]).
    TrackMove {
        clip_id: String,
        track_before: String,
        track_after: String,
        window: ClipWindow,
    },
    /// One clip marker was added (`before` is `None`), removed (`after` is `None`) or replaced. A
    /// marker that leaves with a removed clip is listed here, after the [`ClipChange::Remove`], and
    /// `window` is the clip's last window. A clip marker on an inserted clip is inside its
    /// [`ClipChange::Insert`].
    ClipMarker {
        clip_id: String,
        track_id: String,
        window: ClipWindow,
        marker_id: String,
        before: Option<Box<StudioMarker>>,
        after: Option<Box<StudioMarker>>,
    },
    /// One transition slot of the clip changed, including a transition pruned by the edit. Listed for
    /// a removed clip as for [`ClipChange::ClipMarker`]. Both of those are also in
    /// [`EditProposal::dropped_transitions`].
    ClipTransition {
        clip_id: String,
        track_id: String,
        window: ClipWindow,
        slot: TransitionSlot,
        before: Option<Box<TimelineTransition>>,
        after: Option<Box<TimelineTransition>>,
    },
    /// A clip member no planner writes today: `enabled`, `source_ref` or `marker_order` (the same
    /// markers in another order). The values are in the proposal's snapshot and its inverse.
    ClipField {
        clip_id: String,
        track_id: String,
        window: ClipWindow,
        field: &'static str,
    },
    /// A track member no planner writes today: a stored setting by its Folio name, or `presence`
    /// (the track was added or removed).
    TrackField {
        track_id: String,
        field: &'static str,
    },
    /// One sequence marker was added (`before` is `None`), removed (`after` is `None`) or replaced.
    SequenceMarker {
        sequence_id: String,
        marker_id: String,
        before: Option<Box<StudioMarker>>,
        after: Option<Box<StudioMarker>>,
    },
    /// The work area was set, moved or cleared.
    WorkArea {
        sequence_id: String,
        before: Option<WorkArea>,
        after: Option<WorkArea>,
    },
    /// A sequence member no planner writes today: `name`, `frame_rate`, `marker_order` or
    /// `track_order`.
    SequenceField {
        sequence_id: String,
        field: &'static str,
    },
}

impl ClipChange {
    /// Stable snake_case name of this kind of change.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Retime { .. } => "retime",
            Self::Insert { .. } => "insert",
            Self::Remove { .. } => "remove",
            Self::RetimeRecord { .. } => "retime_record",
            Self::Relink { .. } => "relink",
            Self::Speed { .. } => "speed",
            Self::TrackMove { .. } => "track_move",
            Self::ClipMarker { .. } => "clip_marker",
            Self::ClipTransition { .. } => "clip_transition",
            Self::ClipField { .. } => "clip_field",
            Self::TrackField { .. } => "track_field",
            Self::SequenceMarker { .. } => "sequence_marker",
            Self::WorkArea { .. } => "work_area",
            Self::SequenceField { .. } => "sequence_field",
        }
    }
    /// The clip this change belongs to; `None` for a track-level or sequence-level change.
    pub fn clip_id(&self) -> Option<&str> {
        match self {
            Self::Retime { clip_id, .. }
            | Self::Remove { clip_id, .. }
            | Self::RetimeRecord { clip_id, .. }
            | Self::Relink { clip_id, .. }
            | Self::Speed { clip_id, .. }
            | Self::TrackMove { clip_id, .. }
            | Self::ClipMarker { clip_id, .. }
            | Self::ClipTransition { clip_id, .. }
            | Self::ClipField { clip_id, .. } => Some(clip_id),
            Self::Insert { clip, .. } => Some(&clip.clip_id),
            Self::TrackField { .. }
            | Self::SequenceMarker { .. }
            | Self::WorkArea { .. }
            | Self::SequenceField { .. } => None,
        }
    }
    /// The clip's track after the edit (before it, for a removal), or the track a track-level change
    /// names; `None` for a sequence-level change.
    pub fn track_id(&self) -> Option<&str> {
        match self {
            Self::Retime { track_id, .. }
            | Self::Insert { track_id, .. }
            | Self::Remove { track_id, .. }
            | Self::RetimeRecord { track_id, .. }
            | Self::Relink { track_id, .. }
            | Self::Speed { track_id, .. }
            | Self::ClipMarker { track_id, .. }
            | Self::ClipTransition { track_id, .. }
            | Self::ClipField { track_id, .. }
            | Self::TrackField { track_id, .. } => Some(track_id),
            Self::TrackMove { track_after, .. } => Some(track_after),
            Self::SequenceMarker { .. } | Self::WorkArea { .. } | Self::SequenceField { .. } => None,
        }
    }
    pub fn before(&self) -> Option<ClipWindow> {
        match self {
            Self::Retime { before, .. } | Self::Remove { before, .. } => Some(*before),
            Self::RetimeRecord { window, .. }
            | Self::Relink { window, .. }
            | Self::Speed { window, .. }
            | Self::TrackMove { window, .. }
            | Self::ClipMarker { window, .. }
            | Self::ClipTransition { window, .. }
            | Self::ClipField { window, .. } => Some(*window),
            Self::Insert { .. }
            | Self::TrackField { .. }
            | Self::SequenceMarker { .. }
            | Self::WorkArea { .. }
            | Self::SequenceField { .. } => None,
        }
    }
    pub fn after(&self) -> Option<ClipWindow> {
        match self {
            Self::Retime { after, .. } => Some(*after),
            Self::RetimeRecord { window, .. }
            | Self::Relink { window, .. }
            | Self::Speed { window, .. }
            | Self::TrackMove { window, .. }
            | Self::ClipMarker { window, .. }
            | Self::ClipTransition { window, .. }
            | Self::ClipField { window, .. } => Some(*window),
            Self::Insert { clip, .. } => Some(window(clip)),
            Self::Remove { .. }
            | Self::TrackField { .. }
            | Self::SequenceMarker { .. }
            | Self::WorkArea { .. }
            | Self::SequenceField { .. } => None,
        }
    }
    /// Every id this change writes, for the write footprint: the record, the record holding a marker
    /// or transition, a track whose clip list changed. The work area has no id of its own: it is
    /// written as a member of its sequence.
    pub(crate) fn write_ids(&self) -> Vec<&str> {
        match self {
            Self::Retime { clip_id, .. }
            | Self::RetimeRecord { clip_id, .. }
            | Self::Relink { clip_id, .. }
            | Self::Speed { clip_id, .. }
            | Self::ClipField { clip_id, .. } => vec![clip_id],
            Self::Insert { track_id, clip } => {
                let markers = clip.markers.iter().map(|m| m.marker_id.as_str());
                let slots = [&clip.transitions.head, &clip.transitions.tail];
                let transitions = slots.into_iter().flatten().map(|t| t.transition_id.as_str());
                [clip.clip_id.as_str(), track_id.as_str()].into_iter().chain(markers).chain(transitions).collect()
            }
            Self::Remove { clip_id, track_id, .. } => vec![clip_id, track_id],
            Self::TrackMove { clip_id, track_before, track_after, .. } => {
                vec![clip_id, track_before, track_after]
            }
            Self::ClipMarker { clip_id, marker_id, .. } => vec![clip_id, marker_id],
            Self::ClipTransition { clip_id, before, after, .. } => {
                let held = [before, after].into_iter().flatten().map(|t| t.transition_id.as_str());
                std::iter::once(clip_id.as_str()).chain(held).collect()
            }
            Self::TrackField { track_id, .. } => vec![track_id],
            Self::SequenceMarker { sequence_id, marker_id, .. } => vec![sequence_id, marker_id],
            Self::WorkArea { sequence_id, .. } | Self::SequenceField { sequence_id, .. } => {
                vec![sequence_id]
            }
        }
    }
}

/// One edit against exactly one base revision, with the Folio-validated prospective snapshot.
#[derive(Clone, Debug)]
pub struct EditProposal {
    sequence_id: String,
    base_revision: u64,
    target_revision: u64,
    changes: Vec<ClipChange>,
    read_footprint: Vec<String>,
    write_footprint: Vec<String>,
    snapshot: Snapshot,
    inverse: TimelineInverse,
    splits: Vec<(String, String)>,
    dropped_transitions: Vec<String>,
    removed_markers: Vec<String>,
}

/// The reversible half of one applied proposal: the sequence as the edit left it (`expect`, the
/// precondition) and as it was before (`restore`). Pulse holds no history; the host keeps one of
/// these per history step and plans undo/redo with [`plan_timeline_revert`].
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineInverse {
    sequence_id: String,
    expect: StudioSequence,
    restore: StudioSequence,
}

impl TimelineInverse {
    pub fn sequence_id(&self) -> &str {
        &self.sequence_id
    }
    pub fn expect(&self) -> &StudioSequence {
        &self.expect
    }
    pub fn restore(&self) -> &StudioSequence {
        &self.restore
    }
}

impl EditProposal {
    pub fn sequence_id(&self) -> &str {
        &self.sequence_id
    }
    pub fn base_revision(&self) -> u64 {
        self.base_revision
    }
    pub fn target_revision(&self) -> u64 {
        self.target_revision
    }
    pub fn changes(&self) -> &[ClipChange] {
        &self.changes
    }
    pub fn change_for(&self, clip_id: &str) -> Option<&ClipChange> {
        self.changes.iter().find(|c| c.clip_id() == Some(clip_id))
    }
    pub fn read_footprint(&self) -> &[String] {
        &self.read_footprint
    }
    pub fn write_footprint(&self) -> &[String] {
        &self.write_footprint
    }
    /// The prospective document, already validated by Folio at `target_revision`.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    /// `(origin clip id, new clip id)` for every clip this edit cut in two (razor, range lift and
    /// extract, insert and overwrite); annotation owners use it to carry clip markers.
    pub fn splits(&self) -> &[(String, String)] {
        &self.splits
    }
    /// Transitions this edit took out of the sequence: made unplayable (the incoming clip gone, the
    /// clips no longer abutting, a handle now short) or gone with their removed clip. Listed, never
    /// silently lost (tests `every_transition_an_edit_takes_out_is_listed_as_dropped`,
    /// `a_transition_that_leaves_with_its_outgoing_clip_is_listed`); undo restores them. A transition
    /// set or cleared by name is not listed.
    pub fn dropped_transitions(&self) -> &[String] {
        &self.dropped_transitions
    }
    /// Markers that left the sequence with this edit (a removed clip, or a removed marker).
    pub fn removed_markers(&self) -> &[String] {
        &self.removed_markers
    }
    /// Undo of this proposal for [`plan_timeline_revert`] (once it is applied).
    pub fn inverse(&self) -> &TimelineInverse {
        &self.inverse
    }
}

#[derive(Default)]
pub(crate) struct Footprint {
    seen: BTreeSet<String>,
    ids: Vec<String>,
    /// `(origin clip id, new clip id)` for every clip the edit cut in two.
    pub(crate) splits: Vec<(String, String)>,
}
impl Footprint {
    pub(crate) fn add(&mut self, id: &str) {
        if self.seen.insert(id.to_owned()) {
            self.ids.push(id.to_owned());
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edge {
    Head,
    Tail,
}

pub(crate) fn rate_of(seq: &StudioSequence) -> Result<FrameRate> {
    seq.frame_rate
        .accord()
        .ok_or(PulseError::UnsupportedFrameRate)
}

pub(crate) fn grid_of(track: &StudioTrack, rate: FrameRate) -> Grid {
    if track.kind == TrackKind::Video {
        Grid::Frames(rate)
    } else {
        Grid::Free
    }
}

pub(crate) fn lane_of(track: &StudioTrack) -> Result<Vec<TickRange>> {
    track
        .clips
        .iter()
        .map(|c| TickRange::from_start_duration(c.timeline_start, c.duration))
        .collect()
}

fn locate(seq: &StudioSequence, clip_id: &str) -> Result<(usize, usize)> {
    seq.tracks
        .iter()
        .enumerate()
        .find_map(|(t, track)| {
            track
                .clips
                .iter()
                .position(|c| c.clip_id == clip_id)
                .map(|c| (t, c))
        })
        .ok_or(PulseError::UnknownClip)
}

/// The clip plus its whole link group, primary first; one clip per track.
fn selection(seq: &StudioSequence, primary: (usize, usize)) -> Result<Vec<(usize, usize)>> {
    let mut members = vec![primary];
    if let Some(group) = &seq.tracks[primary.0].clips[primary.1].link_group {
        for (t, track) in seq.tracks.iter().enumerate() {
            for (c, clip) in track.clips.iter().enumerate() {
                if (t, c) != primary && clip.link_group.as_ref() == Some(group) {
                    members.push((t, c));
                }
            }
        }
    }
    let tracks: BTreeSet<usize> = members.iter().map(|(t, _)| *t).collect();
    if tracks.len() != members.len() {
        return Err(PulseError::LinkedEditUnsupported);
    }
    Ok(members)
}

fn require_unlocked(
    seq: &StudioSequence,
    members: &[(usize, usize)],
    reads: &mut Footprint,
) -> Result<()> {
    for (t, c) in members {
        reads.add(&seq.tracks[*t].track_id);
        reads.add(&seq.tracks[*t].clips[*c].clip_id);
    }
    if members.iter().any(|(t, _)| seq.tracks[*t].locked) {
        return Err(PulseError::TrackLocked);
    }
    Ok(())
}

/// Every link group touched by `involved` must be wholly inside it, else a partner would desync.
fn require_closed(seq: &StudioSequence, involved: &[(usize, usize)]) -> Result<()> {
    let set: BTreeSet<(usize, usize)> = involved.iter().copied().collect();
    let groups: BTreeSet<&str> = involved
        .iter()
        .filter_map(|(t, c)| seq.tracks[*t].clips[*c].link_group.as_deref())
        .collect();
    for (t, track) in seq.tracks.iter().enumerate() {
        for (c, clip) in track.clips.iter().enumerate() {
            if clip
                .link_group
                .as_deref()
                .is_some_and(|g| groups.contains(g))
                && !set.contains(&(t, c))
            {
                return Err(PulseError::LinkedEditUnsupported);
            }
        }
    }
    Ok(())
}

fn underflow_is_outside(error: PulseError) -> PulseError {
    if error == PulseError::Underflow {
        PulseError::OutsideRange
    } else {
        error
    }
}

pub(crate) fn check_extent(clip: &StudioClip, new_out: Ticks, ctx: &EditContext) -> Result<()> {
    match ctx.extents.extent(&clip.source_ref) {
        None => Err(PulseError::SourceExtentUnknown),
        Some(extent) if new_out > extent => Err(PulseError::PastSourceEnd),
        Some(_) => Ok(()),
    }
}

/// Moves the source bound that backs `edge` so the source window matches `new_duration`
/// (reversal flips which bound a timeline edge reads from; bounds never swap, STU-VID-020).
fn retime_edge(
    clip: &mut StudioClip,
    edge: Edge,
    new_duration: Ticks,
    ctx: &EditContext,
) -> Result<()> {
    let change = TickDelta::between(clip.duration, new_duration).value();
    if change == 0 {
        return Ok(());
    }
    let magnitude = u64::try_from(change.unsigned_abs()).map_err(|_| PulseError::Overflow)?;
    let span = i128::from(clip.speed.span_for_duration(Ticks::new(magnitude))?.value());
    let signed = TickDelta::new(if change < 0 { -span } else { span })?;
    let moves_out = matches!(
        (edge, clip.speed.is_reversed()),
        (Edge::Tail, false) | (Edge::Head, true)
    );
    if moves_out {
        let new_out = offset(clip.source_out, signed).map_err(underflow_is_outside)?;
        if signed.value() > 0 {
            check_extent(clip, new_out, ctx)?;
        }
        clip.source_out = new_out;
    } else {
        clip.source_in = offset(clip.source_in, signed.negated()).map_err(|e| match e {
            PulseError::Underflow => PulseError::PastSourceStart,
            other => other,
        })?;
    }
    Ok(())
}

/// Writes a same-length lane result back; clips whose duration changed need an edge hint.
fn write_lane(
    track: &mut StudioTrack,
    lane: &[TickRange],
    edges: &BTreeMap<usize, Edge>,
    ctx: &EditContext,
) -> Result<()> {
    if lane.len() != track.clips.len() {
        return Err(PulseError::InvalidInput);
    }
    for (index, (clip, range)) in track.clips.iter_mut().zip(lane).enumerate() {
        if range.duration() != clip.duration {
            let edge = *edges.get(&index).ok_or(PulseError::InvalidInput)?;
            retime_edge(clip, edge, range.duration(), ctx)?;
        }
        clip.timeline_start = range.start();
        clip.duration = range.duration();
    }
    Ok(())
}

/// Ripple on tracks that hold no selected clip: sync-locked tracks shift their later content.
pub(crate) fn shift_other_tracks(
    seq: &StudioSequence,
    out: &mut StudioSequence,
    members: &[(usize, usize)],
    point: Ticks,
    shift: TickDelta,
    ctx: &EditContext,
    reads: &mut Footprint,
) -> Result<()> {
    if shift == TickDelta::ZERO {
        return Ok(());
    }
    let rate = rate_of(seq)?;
    for (t, track) in seq.tracks.iter().enumerate() {
        ctx.cancel.check()?;
        if !track.sync_locked || members.iter().any(|(m, _)| *m == t) {
            continue;
        }
        let lane = lane_of(track)?;
        if lane.partition_point(|r| r.start().value() < point.value()) >= lane.len() {
            continue;
        }
        reads.add(&track.track_id);
        if track.locked {
            return Err(PulseError::SyncLockBlocked);
        }
        grid_of(track, rate).check_delta(shift)?;
        let shifted = ripple_shift(&lane, point, shift, ctx.cancel).map_err(|e| match e {
            PulseError::Overlap | PulseError::Underflow => PulseError::SyncLockBlocked,
            other => other,
        })?;
        write_lane(&mut out.tracks[t], &shifted, &BTreeMap::new(), ctx)?;
    }
    Ok(())
}

fn trim(
    seq: &StudioSequence,
    clip_id: &str,
    edge: Edge,
    target: Ticks,
    ripple: bool,
    ctx: &EditContext,
    reads: &mut Footprint,
) -> Result<StudioSequence> {
    let rate = rate_of(seq)?;
    let primary = locate(seq, clip_id)?;
    let members = selection(seq, primary)?;
    require_unlocked(seq, &members, reads)?;
    let lead = &seq.tracks[primary.0].clips[primary.1];
    let delta = match edge {
        Edge::Tail => TickDelta::between(lead.duration, target),
        Edge::Head => TickDelta::between(lead.timeline_start, target),
    };
    let point = lead.timeline_end().ok_or(PulseError::Overflow)?;
    let mut out = seq.clone();
    for &(t, c) in &members {
        let track = &seq.tracks[t];
        let clip = &track.clips[c];
        let (lane, grid) = (lane_of(track)?, grid_of(track, rate));
        let lane = match (edge, ripple) {
            (Edge::Tail, false) => {
                let d = offset(clip.duration, delta).map_err(underflow_is_outside)?;
                trim_end(&lane, c, d, grid, ctx.cancel)?
            }
            (Edge::Tail, true) => {
                let d = offset(clip.duration, delta).map_err(underflow_is_outside)?;
                ripple_trim_end(&lane, c, d, grid, ctx.cancel)?
            }
            (Edge::Head, false) => {
                let h = offset(clip.timeline_start, delta)?;
                trim_start(&lane, c, h, grid, ctx.cancel)?
            }
            (Edge::Head, true) => {
                let h = offset(clip.timeline_start, delta)?;
                ripple_trim_start(&lane, c, h, grid, ctx.cancel)?
            }
        };
        write_lane(&mut out.tracks[t], &lane, &BTreeMap::from([(c, edge)]), ctx)?;
    }
    if ripple {
        let shift = if edge == Edge::Tail {
            delta
        } else {
            delta.negated()
        };
        shift_other_tracks(seq, &mut out, &members, point, shift, ctx, reads)?;
    }
    Ok(out)
}

fn razor(
    seq: &StudioSequence,
    clip_id: &str,
    at: Ticks,
    new_id: &str,
    linked: Option<&LinkedRazor>,
    ctx: &EditContext,
    reads: &mut Footprint,
) -> Result<StudioSequence> {
    ctx.cancel.check()?;
    let rate = rate_of(seq)?;
    let primary = locate(seq, clip_id)?;
    let members = selection(seq, primary)?;
    require_unlocked(seq, &members, reads)?;
    let group = seq.tracks[primary.0].clips[primary.1].link_group.as_deref();
    // Right-half id per member (primary first) and the link group the right halves form.
    let mut ids: Vec<&str> = vec![new_id];
    let right_group = match (group, linked) {
        (None, None) => None,
        (None, Some(_)) => return Err(PulseError::InvalidInput),
        (Some(_), None) => return Err(PulseError::InsufficientIds),
        (Some(old), Some(extra)) => {
            if extra.right_clip_ids.len() != members.len() - 1 {
                return Err(PulseError::InsufficientIds);
            }
            for &(t, c) in &members[1..] {
                let existing = &seq.tracks[t].clips[c].clip_id;
                let (_, fresh) = extra
                    .right_clip_ids
                    .iter()
                    .find(|(member, _)| member == existing)
                    .ok_or(PulseError::InsufficientIds)?;
                ids.push(fresh);
            }
            let in_use = seq
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .any(|k| k.link_group.as_deref() == Some(extra.right_link_group.as_str()));
            if in_use || extra.right_link_group == old {
                return Err(PulseError::DuplicateId);
            }
            Some(extra.right_link_group.as_str())
        }
    };
    let taken: BTreeSet<&str> = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|k| k.clip_id.as_str())
        .collect();
    let fresh: BTreeSet<&str> = ids.iter().copied().collect();
    if fresh.len() != ids.len() || ids.iter().any(|id| taken.contains(id)) {
        return Err(PulseError::DuplicateId);
    }
    let mut out = seq.clone();
    for (&(t, c), fresh_id) in members.iter().zip(&ids) {
        let track = &seq.tracks[t];
        let clip = &track.clips[c];
        TickRange::from_start_duration(clip.timeline_start, clip.duration)?.split_at(at)?;
        grid_of(track, rate).check_delta(TickDelta::between(clip.timeline_start, at))?;
        let (first, mut second) = split_clip(clip, at, fresh_id)?;
        reads
            .splits
            .push((clip.clip_id.clone(), (*fresh_id).to_owned()));
        second.link_group = right_group.map(str::to_owned);
        out.tracks[t].clips[c] = first;
        out.tracks[t].clips.insert(c + 1, second);
    }
    Ok(out)
}

/// Splits `clip` at timeline tick `at` (strictly inside it) into two clips that render like the
/// original (STU-VID-027): the right half takes `right_id` and continues the source where the left
/// half stops. Forward plays source_in -> source_out, reversed plays source_out -> source_in.
pub(crate) fn split_clip(
    clip: &StudioClip,
    at: Ticks,
    right_id: &str,
) -> Result<(StudioClip, StudioClip)> {
    let whole = TickRange::from_start_duration(clip.timeline_start, clip.duration)?;
    let (left, right) = whole.split_at(at)?;
    let span = clip.speed.span_for_duration(left.duration())?;
    let ((left_in, left_out), (right_in, right_out)) = if clip.speed.is_reversed() {
        let cut = clip.source_out.checked_sub(span)?;
        ((cut, clip.source_out), (clip.source_in, cut))
    } else {
        let cut = clip.source_in.checked_add(span)?;
        ((clip.source_in, cut), (cut, clip.source_out))
    };
    let mut second = clip.clone();
    second.clip_id = right_id.to_owned();
    second.source_in = right_in;
    second.source_out = right_out;
    second.timeline_start = right.start();
    second.duration = right.duration();
    let mut first = clip.clone();
    first.source_in = left_in;
    first.source_out = left_out;
    first.duration = left.duration();
    crate::annotate::split_records(&mut first, &mut second);
    Ok((first, second))
}

fn neighbour_edit(
    seq: &StudioSequence,
    clip_id: &str,
    delta: TickDelta,
    is_roll: bool,
    ctx: &EditContext,
    reads: &mut Footprint,
) -> Result<StudioSequence> {
    let rate = rate_of(seq)?;
    let anchor = locate(seq, clip_id)?;
    let members = selection(seq, anchor)?;
    require_unlocked(seq, &members, reads)?;
    let mut involved = Vec::new();
    for &(t, c) in &members {
        let len = seq.tracks[t].clips.len();
        let first = if is_roll { c } else { c.saturating_sub(1) };
        involved.extend((first..=c + 1).filter(|i| *i < len).map(|i| (t, i)));
    }
    require_closed(seq, &involved)?;
    let mut out = seq.clone();
    for &(t, c) in &members {
        let track = &seq.tracks[t];
        let (lane, grid) = (lane_of(track)?, grid_of(track, rate));
        let (new_lane, edges) = if is_roll {
            (
                roll_edit(&lane, c, delta, grid, ctx.cancel)?,
                BTreeMap::from([(c, Edge::Tail), (c + 1, Edge::Head)]),
            )
        } else {
            (
                slide(&lane, c, delta, grid, ctx.cancel)?,
                BTreeMap::from([(c.saturating_sub(1), Edge::Tail), (c + 1, Edge::Head)]),
            )
        };
        write_lane(&mut out.tracks[t], &new_lane, &edges, ctx)?;
    }
    Ok(out)
}

/// Rebuilds a track from lane pieces; the second piece of one origin clip takes a fresh id.
pub(crate) fn write_pieces(
    track: &mut StudioTrack,
    pieces: &[LanePiece],
    fresh: &mut std::slice::Iter<'_, String>,
    taken: &mut BTreeSet<String>,
    splits: &mut Vec<(String, String)>,
    ctx: &EditContext,
) -> Result<()> {
    let originals = std::mem::take(&mut track.clips);
    let mut rebuilt: Vec<StudioClip> = Vec::with_capacity(pieces.len());
    let mut seen = BTreeSet::new();
    let mut first_piece: BTreeMap<usize, usize> = BTreeMap::new();
    for piece in pieces {
        let mut clip = originals
            .get(piece.origin)
            .ok_or(PulseError::InvalidInput)?
            .clone();
        let second_piece = !seen.insert(piece.origin);
        if second_piece {
            let id = fresh.next().ok_or(PulseError::InsufficientIds)?;
            if !taken.insert(id.clone()) {
                return Err(PulseError::DuplicateId);
            }
            splits.push((clip.clip_id.clone(), id.clone()));
            clip.clip_id = id.clone();
        }
        let duration = piece.range.duration();
        if piece.tail_cut.value() > 0 {
            retime_edge(&mut clip, Edge::Tail, duration, ctx)?;
        }
        if piece.head_cut.value() > 0 {
            retime_edge(&mut clip, Edge::Head, duration, ctx)?;
        }
        clip.timeline_start = piece.range.start();
        clip.duration = duration;
        match first_piece.get(&piece.origin) {
            Some(&left) if second_piece => {
                crate::annotate::split_records(&mut rebuilt[left], &mut clip);
            }
            _ => {
                first_piece.insert(piece.origin, rebuilt.len());
            }
        }
        rebuilt.push(clip);
    }
    track.clips = rebuilt;
    Ok(())
}

/// Removes `plan[i].1` from track `plan[i].0` (lift) or removes and closes it (extract).
fn cut_tracks(
    seq: &StudioSequence,
    out: &mut StudioSequence,
    plan: &[(usize, TickRange)],
    close: bool,
    fresh: &[String],
    ctx: &EditContext,
    reads: &mut Footprint,
) -> Result<()> {
    let rate = rate_of(seq)?;
    let mut taken: BTreeSet<String> = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|c| c.clip_id.clone())
        .collect();
    let mut ids = fresh.iter();
    for &(t, span) in plan {
        ctx.cancel.check()?;
        let track = &seq.tracks[t];
        let (lane, grid) = (lane_of(track)?, grid_of(track, rate));
        let pieces = if close {
            extract_span(&lane, span, grid, ctx.cancel)?
        } else {
            lift_span(&lane, span, grid, ctx.cancel)?
        };
        write_pieces(
            &mut out.tracks[t],
            &pieces,
            &mut ids,
            &mut taken,
            &mut reads.splits,
            ctx,
        )?;
    }
    Ok(())
}

fn remove_clips(
    seq: &StudioSequence,
    clip_id: &str,
    close: bool,
    ctx: &EditContext,
    reads: &mut Footprint,
) -> Result<StudioSequence> {
    let primary = locate(seq, clip_id)?;
    let members = selection(seq, primary)?;
    require_unlocked(seq, &members, reads)?;
    let mut plan = Vec::new();
    for &(t, c) in &members {
        let clip = &seq.tracks[t].clips[c];
        plan.push((
            t,
            TickRange::from_start_duration(clip.timeline_start, clip.duration)?,
        ));
    }
    let mut out = seq.clone();
    cut_tracks(seq, &mut out, &plan, close, &[], ctx, reads)?;
    if close {
        let lead = plan[0].1;
        let shift = TickDelta::between(lead.end(), lead.start());
        shift_other_tracks(seq, &mut out, &members, lead.end(), shift, ctx, reads)?;
    }
    Ok(out)
}

fn cut_range(
    seq: &StudioSequence,
    span: TickRange,
    close: bool,
    fresh: &[String],
    ctx: &EditContext,
    reads: &mut Footprint,
) -> Result<StudioSequence> {
    let targeted: Vec<usize> = (0..seq.tracks.len())
        .filter(|t| seq.tracks[*t].targeted)
        .collect();
    if targeted.is_empty() {
        return Err(PulseError::InvalidInput);
    }
    for &t in &targeted {
        let track = &seq.tracks[t];
        reads.add(&track.track_id);
        if track.locked {
            return Err(PulseError::TrackLocked);
        }
        let overlapped_link = track.clips.iter().any(|c| {
            c.link_group.is_some()
                && c.timeline_start < span.end()
                && c.timeline_end().is_none_or(|end| end > span.start())
        });
        if overlapped_link {
            return Err(PulseError::LinkedEditUnsupported);
        }
    }
    let plan: Vec<(usize, TickRange)> = targeted.iter().map(|t| (*t, span)).collect();
    let mut out = seq.clone();
    cut_tracks(seq, &mut out, &plan, close, fresh, ctx, reads)?;
    if close {
        let edited: Vec<(usize, usize)> = targeted.iter().map(|t| (*t, 0)).collect();
        let shift = TickDelta::between(span.end(), span.start());
        shift_other_tracks(seq, &mut out, &edited, span.end(), shift, ctx, reads)?;
    }
    Ok(out)
}

fn close_gap(
    seq: &StudioSequence,
    track_id: &str,
    at: Ticks,
    ctx: &EditContext,
    reads: &mut Footprint,
) -> Result<StudioSequence> {
    let rate = rate_of(seq)?;
    let t = seq
        .tracks
        .iter()
        .position(|t| t.track_id == track_id)
        .ok_or(PulseError::UnknownTrack)?;
    let track = &seq.tracks[t];
    reads.add(&track.track_id);
    if track.locked {
        return Err(PulseError::TrackLocked);
    }
    let lane = lane_of(track)?;
    let gap = gaps(&lane, ctx.cancel)?
        .into_iter()
        .find(|g| g.contains(at))
        .ok_or(PulseError::OutsideRange)?;
    let shift = TickDelta::between(gap.end(), gap.start());
    grid_of(track, rate).check_delta(shift)?;
    let shifted = ripple_shift(&lane, gap.end(), shift, ctx.cancel)?;
    let mut out = seq.clone();
    write_lane(&mut out.tracks[t], &shifted, &BTreeMap::new(), ctx)?;
    shift_other_tracks(seq, &mut out, &[(t, 0)], gap.end(), shift, ctx, reads)?;
    Ok(out)
}

fn slip(
    seq: &StudioSequence,
    clip_id: &str,
    delta: TickDelta,
    ctx: &EditContext,
    reads: &mut Footprint,
) -> Result<StudioSequence> {
    let primary = locate(seq, clip_id)?;
    let members = selection(seq, primary)?;
    require_unlocked(seq, &members, reads)?;
    let mut out = seq.clone();
    for &(t, c) in &members {
        let clip = &mut out.tracks[t].clips[c];
        let past_start = |e: PulseError| {
            if e == PulseError::Underflow {
                PulseError::PastSourceStart
            } else {
                e
            }
        };
        let new_in = offset(clip.source_in, delta).map_err(past_start)?;
        let new_out = offset(clip.source_out, delta).map_err(past_start)?;
        if delta.value() > 0 {
            check_extent(clip, new_out, ctx)?;
        }
        clip.source_in = new_in;
        clip.source_out = new_out;
    }
    Ok(out)
}

pub(crate) fn apply_edit(
    seq: &StudioSequence,
    edit: &TimelineEdit,
    ctx: &EditContext,
    reads: &mut Footprint,
) -> Result<StudioSequence> {
    match edit {
        TimelineEdit::TrimOut {
            clip_id,
            new_duration,
            ripple,
        } => trim(seq, clip_id, Edge::Tail, *new_duration, *ripple, ctx, reads),
        TimelineEdit::TrimIn {
            clip_id,
            new_head,
            ripple,
        } => trim(seq, clip_id, Edge::Head, *new_head, *ripple, ctx, reads),
        TimelineEdit::Razor {
            clip_id,
            at,
            new_right_clip_id,
            linked,
        } => razor(
            seq,
            clip_id,
            *at,
            new_right_clip_id,
            linked.as_ref(),
            ctx,
            reads,
        ),
        TimelineEdit::LiftClip { clip_id } => remove_clips(seq, clip_id, false, ctx, reads),
        TimelineEdit::ExtractClip { clip_id } => remove_clips(seq, clip_id, true, ctx, reads),
        TimelineEdit::LiftRange {
            span,
            fresh_clip_ids,
        } => cut_range(seq, *span, false, fresh_clip_ids, ctx, reads),
        TimelineEdit::ExtractRange {
            span,
            fresh_clip_ids,
        } => cut_range(seq, *span, true, fresh_clip_ids, ctx, reads),
        TimelineEdit::CloseGap { track_id, at } => close_gap(seq, track_id, *at, ctx, reads),
        TimelineEdit::Roll {
            left_clip_id,
            delta,
        } => neighbour_edit(seq, left_clip_id, *delta, true, ctx, reads),
        TimelineEdit::Slide { clip_id, delta } => {
            neighbour_edit(seq, clip_id, *delta, false, ctx, reads)
        }
        TimelineEdit::Slip { clip_id, delta } => slip(seq, clip_id, *delta, ctx, reads),
    }
}

fn from_diagnostic(diagnostic: Diagnostic) -> PulseError {
    match diagnostic.code {
        Code::Canceled => PulseError::Canceled,
        Code::BudgetExceeded => PulseError::LimitExceeded,
        Code::StaleRevision => PulseError::StaleRevision,
        Code::RevisionOverflow => PulseError::Overflow,
        _ => PulseError::FolioRejected,
    }
}

/// Pairs two marker lists by id and reports each marker added, removed or replaced. Answers whether
/// the markers both lists hold sit in another order.
fn diff_markers<'a>(
    old: &'a [StudioMarker],
    new: &'a [StudioMarker],
    mut report: impl FnMut(&'a str, Option<&'a StudioMarker>, Option<&'a StudioMarker>),
) -> bool {
    if old == new {
        return false;
    }
    let by_id = |list: &'a [StudioMarker]| -> BTreeMap<&'a str, &'a StudioMarker> {
        list.iter().map(|m| (m.marker_id.as_str(), m)).collect()
    };
    let (before, after) = (by_id(old), by_id(new));
    for marker in old {
        if !after.contains_key(marker.marker_id.as_str()) {
            report(&marker.marker_id, Some(marker), None);
        }
    }
    for marker in new {
        match before.get(marker.marker_id.as_str()) {
            None => report(&marker.marker_id, None, Some(marker)),
            Some(was) if *was != marker => report(&marker.marker_id, Some(*was), Some(marker)),
            Some(_) => {}
        }
    }
    let kept_old = old.iter().map(|m| m.marker_id.as_str()).filter(|id| after.contains_key(id));
    let kept_new = new.iter().map(|m| m.marker_id.as_str()).filter(|id| before.contains_key(id));
    !kept_old.eq(kept_new)
}

fn boxed<T: Clone>(record: Option<&T>) -> Option<Box<T>> {
    record.map(|r| Box::new(r.clone()))
}

/// Every difference between two values of one clip.
fn diff_clip(
    track_before: &str,
    track_id: &str,
    old: &StudioClip,
    new: &StudioClip,
    changes: &mut Vec<ClipChange>,
) {
    // No `..`: a new clip member stops compiling here until someone decides how it is reported.
    let StudioClip {
        schema_id: _, // one token per schema, cannot differ
        clip_id,      // the key this pair was found by
        source_ref,
        source_in: _,      // `window`
        source_out: _,     // `window`
        timeline_start: _, // `window`
        duration: _,       // `window`
        speed,
        enabled,
        link_group,
        markers,
        transitions,
        retime,
    } = new;
    let ClipTransitions { head, tail } = transitions;
    let at = window(new);
    let (clip, track) = (clip_id.clone(), track_id.to_owned());
    if window(old) != at {
        changes.push(ClipChange::Retime { clip_id: clip.clone(), track_id: track.clone(), before: window(old), after: at });
    }
    if track_before != track_id {
        changes.push(ClipChange::TrackMove {
            clip_id: clip.clone(),
            track_before: track_before.to_owned(),
            track_after: track.clone(),
            window: at,
        });
    }
    if old.speed != *speed {
        changes.push(ClipChange::Speed { clip_id: clip.clone(), track_id: track.clone(), window: at, before: old.speed, after: *speed });
    }
    if old.retime != *retime {
        changes.push(ClipChange::RetimeRecord { clip_id: clip.clone(), track_id: track.clone(), window: at, before: old.retime, after: *retime });
    }
    if old.link_group != *link_group {
        changes.push(ClipChange::Relink {
            clip_id: clip.clone(),
            track_id: track.clone(),
            window: at,
            group_before: old.link_group.clone(),
            group_after: link_group.clone(),
        });
    }
    let field = |field| ClipChange::ClipField { clip_id: clip.clone(), track_id: track.clone(), window: at, field };
    if old.enabled != *enabled {
        changes.push(field("enabled"));
    }
    if old.source_ref != *source_ref {
        changes.push(field("source_ref"));
    }
    let reordered = diff_markers(&old.markers, markers, |marker_id, before, after| {
        changes.push(ClipChange::ClipMarker {
            clip_id: clip.clone(),
            track_id: track.clone(),
            window: at,
            marker_id: marker_id.to_owned(),
            before: boxed(before),
            after: boxed(after),
        });
    });
    if reordered {
        changes.push(field("marker_order"));
    }
    let slots = [
        (TransitionSlot::Head, &old.transitions.head, head),
        (TransitionSlot::Tail, &old.transitions.tail, tail),
    ];
    for (slot, before, after) in slots {
        if before != after {
            changes.push(ClipChange::ClipTransition {
                clip_id: clip.clone(),
                track_id: track.clone(),
                window: at,
                slot,
                before: boxed(before.as_ref()),
                after: boxed(after.as_ref()),
            });
        }
    }
}

/// A clip that left, then the markers and transitions that left with it.
fn removed_clip(track_id: &str, clip: &StudioClip, changes: &mut Vec<ClipChange>) {
    let at = window(clip);
    changes.push(ClipChange::Remove { clip_id: clip.clip_id.clone(), track_id: track_id.to_owned(), before: at });
    for marker in &clip.markers {
        changes.push(ClipChange::ClipMarker {
            clip_id: clip.clip_id.clone(),
            track_id: track_id.to_owned(),
            window: at,
            marker_id: marker.marker_id.clone(),
            before: Some(Box::new(marker.clone())),
            after: None,
        });
    }
    let slots = [(TransitionSlot::Head, &clip.transitions.head), (TransitionSlot::Tail, &clip.transitions.tail)];
    for (slot, transition) in slots {
        if let Some(transition) = transition {
            changes.push(ClipChange::ClipTransition {
                clip_id: clip.clip_id.clone(),
                track_id: track_id.to_owned(),
                window: at,
                slot,
                before: Some(Box::new(transition.clone())),
                after: None,
            });
        }
    }
}

/// The track's own members; its clips are records of their own.
fn diff_track(old: &StudioTrack, new: &StudioTrack, changes: &mut Vec<ClipChange>) {
    // No `..`: a new track member stops compiling here until someone decides how it is reported.
    let StudioTrack {
        schema_id: _, // one token per schema, cannot differ
        track_id,     // the key this pair was found by
        kind,
        index,
        name,
        enabled,
        locked,
        sync_locked,
        targeted,
        muted,
        soloed,
        clips: _, // `diff` walks them by clip id
        volume,
        pan,
    } = new;
    let fields = [
        ("kind", old.kind != *kind),
        ("index", old.index != *index),
        ("name", old.name != *name),
        ("enabled", old.enabled != *enabled),
        ("locked", old.locked != *locked),
        ("sync_locked", old.sync_locked != *sync_locked),
        ("targeted", old.targeted != *targeted),
        ("muted", old.muted != *muted),
        ("soloed", old.soloed != *soloed),
        ("volume", old.volume != *volume),
        ("pan", old.pan != *pan),
    ];
    for (field, differs) in fields {
        if differs {
            changes.push(ClipChange::TrackField { track_id: track_id.clone(), field });
        }
    }
}

/// Every difference between the base and the prospective sequence. Its length is bounded by the
/// document's limits: a fixed number of entries per clip and track, one per marker.
fn diff(old: &StudioSequence, new: &StudioSequence) -> Vec<ClipChange> {
    // No `..`: a new sequence member stops compiling here until someone decides how it is reported.
    let StudioSequence {
        schema_id: _, // one token per schema, cannot differ
        sequence_id,  // the key this pair was found by
        name,
        frame_rate,
        duration: _, // derived from the clips; `propose` names the sequence in the footprint
        tracks,
        markers,
        work_area,
    } = new;
    let tracks_before: BTreeMap<&str, &StudioTrack> = old.tracks.iter().map(|t| (t.track_id.as_str(), t)).collect();
    let tracks_after: BTreeMap<&str, &StudioTrack> = tracks.iter().map(|t| (t.track_id.as_str(), t)).collect();
    let before: BTreeMap<&str, (&str, &StudioClip)> = old
        .tracks
        .iter()
        .flat_map(|t| {
            t.clips
                .iter()
                .map(move |c| (c.clip_id.as_str(), (t.track_id.as_str(), c)))
        })
        .collect();
    let mut seen = BTreeSet::new();
    let mut changes = Vec::new();
    for track in tracks {
        match tracks_before.get(track.track_id.as_str()) {
            Some(old_track) => diff_track(old_track, track, &mut changes),
            None => changes.push(ClipChange::TrackField { track_id: track.track_id.clone(), field: "presence" }),
        }
        for clip in &track.clips {
            seen.insert(clip.clip_id.as_str());
            match before.get(clip.clip_id.as_str()) {
                Some((old_track, old_clip)) => diff_clip(old_track, &track.track_id, old_clip, clip, &mut changes),
                None => changes.push(ClipChange::Insert {
                    track_id: track.track_id.clone(),
                    clip: Box::new(clip.clone()),
                }),
            }
        }
    }
    for track in &old.tracks {
        if !tracks_after.contains_key(track.track_id.as_str()) {
            changes.push(ClipChange::TrackField { track_id: track.track_id.clone(), field: "presence" });
        }
        for clip in &track.clips {
            if !seen.contains(clip.clip_id.as_str()) {
                removed_clip(&track.track_id, clip, &mut changes);
            }
        }
    }
    let reordered = diff_markers(&old.markers, markers, |marker_id, before, after| {
        changes.push(ClipChange::SequenceMarker {
            sequence_id: sequence_id.clone(),
            marker_id: marker_id.to_owned(),
            before: boxed(before),
            after: boxed(after),
        });
    });
    if old.work_area != *work_area {
        changes.push(ClipChange::WorkArea { sequence_id: sequence_id.clone(), before: old.work_area, after: *work_area });
    }
    let kept_old = old.tracks.iter().map(|t| t.track_id.as_str()).filter(|id| tracks_after.contains_key(id));
    let kept_new = tracks.iter().map(|t| t.track_id.as_str()).filter(|id| tracks_before.contains_key(id));
    let fields = [
        ("name", old.name != *name),
        ("frame_rate", old.frame_rate != *frame_rate),
        ("marker_order", reordered),
        ("track_order", !kept_old.eq(kept_new)),
    ];
    for (field, differs) in fields {
        if differs {
            changes.push(ClipChange::SequenceField { sequence_id: sequence_id.clone(), field });
        }
    }
    changes
}

/// Plans one timeline edit against `snapshot` at exactly `expected_revision`. Nothing is
/// committed; the returned snapshot is the prospective document at `expected_revision + 1`,
/// validated by Folio.
pub fn plan_timeline_edit(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    edit: &TimelineEdit,
    ctx: &EditContext,
) -> Result<EditProposal> {
    propose(
        snapshot,
        sequence_id,
        expected_revision,
        ctx,
        true,
        |old, reads| apply_edit(old, edit, ctx, reads),
    )
}

/// Undo or redo of one applied proposal (STU-VID-040 / STU-HIS-002 snapshot revert): restores
/// `inverse.restore()` only while the sequence still equals `inverse.expect()` (the content
/// precondition; anything else is `revert_mismatch`). The result is an ordinary one-step proposal
/// at `expected_revision + 1`, and its own [`EditProposal::inverse`] is the redo.
pub fn plan_timeline_revert(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    inverse: &TimelineInverse,
    ctx: &EditContext,
) -> Result<EditProposal> {
    if inverse.sequence_id != sequence_id {
        return Err(PulseError::UnknownSequence);
    }
    propose(
        snapshot,
        sequence_id,
        expected_revision,
        ctx,
        false,
        |old, reads| {
            for track in &old.tracks {
                reads.add(&track.track_id);
            }
            if *old != inverse.expect {
                return Err(PulseError::RevertMismatch);
            }
            Ok(inverse.restore.clone())
        },
    )
}

/// Shared one-step transaction: stale-revision gate, edit, (with `carry`) removal of transitions
/// that no longer play, Folio validation at revision + 1, change list, footprints and the inverse.
pub(crate) fn propose(
    snapshot: &Snapshot,
    sequence_id: &str,
    expected_revision: u64,
    ctx: &EditContext,
    carry: bool,
    edit: impl FnOnce(&StudioSequence, &mut Footprint) -> Result<StudioSequence>,
) -> Result<EditProposal> {
    ctx.cancel.check()?;
    let document = snapshot.document();
    if document.revision != expected_revision {
        return Err(PulseError::StaleRevision);
    }
    let target_revision = expected_revision
        .checked_add(1)
        .ok_or(PulseError::Overflow)?;
    let index = document
        .sequences
        .iter()
        .position(|s| s.sequence_id == sequence_id)
        .ok_or(PulseError::UnknownSequence)?;
    let old = &document.sequences[index];
    let mut reads = Footprint::default();
    reads.add(sequence_id);
    let mut edited = edit(old, &mut reads)?;
    let dropped_transitions = if carry {
        let mut dropped = crate::transition::prune_transitions(&mut edited, ctx.extents, ctx.cancel)?;
        left_with_their_clips(old, &edited, &mut dropped);
        dropped
    } else {
        Vec::new()
    };
    edited.duration = edited.content_end().ok_or(PulseError::Overflow)?;
    if edited == *old {
        return Err(PulseError::NoChange);
    }
    let mut prospective = document.clone();
    prospective.sequences[index] = edited;
    prospective.revision = target_revision;
    let validated = validate_document(prospective, ctx.budget, ctx.cancel, ctx.resolver)
        .map_err(from_diagnostic)?;
    let new = &validated.document().sequences[index];
    let changes = diff(old, new);
    let mut writes = Footprint::default();
    for change in &changes {
        for id in change.write_ids() {
            writes.add(id);
        }
    }
    if new.duration != old.duration {
        writes.add(sequence_id);
    }
    let kept: BTreeSet<&str> = marker_ids(new).collect();
    let removed_markers = marker_ids(old)
        .filter(|id| !kept.contains(id))
        .map(str::to_owned)
        .collect();
    let inverse = TimelineInverse {
        sequence_id: sequence_id.to_owned(),
        expect: new.clone(),
        restore: old.clone(),
    };
    Ok(EditProposal {
        sequence_id: sequence_id.to_owned(),
        base_revision: expected_revision,
        target_revision,
        changes,
        splits: reads.splits,
        read_footprint: reads.ids,
        write_footprint: writes.ids,
        snapshot: validated,
        inverse,
        dropped_transitions,
        removed_markers,
    })
}

/// Adds the transitions of clips the edit removed: pruning only sees the clips that stayed. One a
/// cut handed to another piece is still in the sequence and is not a loss.
fn left_with_their_clips(old: &StudioSequence, edited: &StudioSequence, dropped: &mut Vec<String>) {
    let clips = || edited.tracks.iter().flat_map(|t| t.clips.iter());
    let live: BTreeSet<&str> = clips().map(|c| c.clip_id.as_str()).collect();
    let held = clips().flat_map(|c| [&c.transitions.head, &c.transitions.tail]).flatten();
    let mut accounted: BTreeSet<String> = held.map(|t| t.transition_id.clone()).collect();
    accounted.extend(dropped.iter().cloned());
    let removed = old.tracks.iter().flat_map(|t| t.clips.iter()).filter(|c| !live.contains(c.clip_id.as_str()));
    for transition in removed.flat_map(|c| [&c.transitions.head, &c.transitions.tail]).flatten() {
        if accounted.insert(transition.transition_id.clone()) {
            dropped.push(transition.transition_id.clone());
        }
    }
}

fn marker_ids(seq: &StudioSequence) -> impl Iterator<Item = &str> {
    seq.markers
        .iter()
        .chain(
            seq.tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .flat_map(|c| c.markers.iter()),
        )
        .map(|m| m.marker_id.as_str())
}

/// The members no planner writes today cannot be reached through a proposal, so they are driven
/// through `diff` directly. `tests/edit_footprint.rs` covers everything a planner can do.
#[cfg(test)]
mod diff_tests {
    use super::*;
    use hsk_studio_folio::{
        ClipSchema, ContentDigest, LabelColor, SequenceFrameRate, SequenceSchema, TrackSchema,
    };

    const SEQ: &str = "seq";

    fn marker(marker_id: &str) -> StudioMarker {
        StudioMarker {
            marker_id: marker_id.into(),
            name: String::new(),
            comment: String::new(),
            color: LabelColor::Rose,
            start: Ticks::new(0),
            duration: Ticks::new(0),
            role: None,
        }
    }
    fn source(manifest: &str) -> ClipSource {
        ClipSource::Media {
            artifact_manifest_id: manifest.into(),
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() },
            asset_id: None,
        }
    }
    fn clip() -> StudioClip {
        StudioClip {
            schema_id: ClipSchema::V1,
            clip_id: "clip".into(),
            source_ref: source("a"),
            source_in: Ticks::new(0),
            source_out: Ticks::new(10),
            timeline_start: Ticks::new(0),
            duration: Ticks::new(10),
            speed: Speed::UNIT,
            enabled: true,
            link_group: None,
            markers: vec![marker("m1"), marker("m2")],
            transitions: ClipTransitions::default(),
            retime: ClipRetime::default(),
        }
    }
    fn track(track_id: &str, clips: Vec<StudioClip>) -> StudioTrack {
        StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: track_id.into(),
            kind: TrackKind::Video,
            index: 0,
            name: String::new(),
            enabled: true,
            locked: false,
            sync_locked: false,
            targeted: false,
            muted: false,
            soloed: false,
            clips,
            volume: 1.0,
            pan: 0.0,
        }
    }
    fn base() -> StudioSequence {
        StudioSequence {
            schema_id: SequenceSchema::V1,
            sequence_id: SEQ.into(),
            name: String::new(),
            frame_rate: SequenceFrameRate { ticks_per_frame: 10 },
            duration: Ticks::new(10),
            tracks: vec![track("t1", vec![clip()]), track("t2", Vec::new())],
            markers: vec![marker("s1"), marker("s2")],
            work_area: None,
        }
    }
    /// `(kind, field or empty, write ids)` of the one entry `edit` must produce.
    fn one(edit: impl FnOnce(&mut StudioSequence)) -> (&'static str, &'static str, Vec<String>) {
        let old = base();
        let mut new = old.clone();
        edit(&mut new);
        let changes = diff(&old, &new);
        assert_eq!(changes.len(), 1, "{changes:?}");
        let field = match &changes[0] {
            ClipChange::ClipField { field, .. }
            | ClipChange::TrackField { field, .. }
            | ClipChange::SequenceField { field, .. } => field,
            _ => "",
        };
        let ids = changes[0].write_ids().into_iter().map(str::to_owned).collect();
        (changes[0].kind(), field, ids)
    }

    #[test]
    fn equal_sequences_report_nothing() {
        assert_eq!(diff(&base(), &base()), []);
    }

    #[test]
    fn clip_members_no_planner_writes_are_reported() {
        let on_clip = |edit: fn(&mut StudioClip)| one(|s| edit(&mut s.tracks[0].clips[0]));
        let clip_field = |field| ("clip_field", field, vec!["clip".to_owned()]);
        assert_eq!(on_clip(|c| c.enabled = false), clip_field("enabled"));
        assert_eq!(on_clip(|c| c.source_ref = source("b")), clip_field("source_ref"));
        assert_eq!(on_clip(|c| c.markers.reverse()), clip_field("marker_order"));
    }

    #[test]
    fn a_clip_on_another_track_names_both_tracks() {
        let moved = one(|s| {
            let c = s.tracks[0].clips.remove(0);
            s.tracks[1].clips.push(c);
        });
        assert_eq!(moved, ("track_move", "", vec!["clip".to_owned(), "t1".to_owned(), "t2".to_owned()]));
    }

    #[test]
    fn every_track_member_is_reported() {
        type Edit = fn(&mut StudioTrack);
        let edits: [(&str, Edit); 11] = [
            ("kind", |t| t.kind = TrackKind::Audio),
            ("index", |t| t.index = 7),
            ("name", |t| t.name = "renamed".into()),
            ("enabled", |t| t.enabled = false),
            ("locked", |t| t.locked = true),
            ("sync_locked", |t| t.sync_locked = true),
            ("targeted", |t| t.targeted = true),
            ("muted", |t| t.muted = true),
            ("soloed", |t| t.soloed = true),
            ("volume", |t| t.volume = 0.5),
            ("pan", |t| t.pan = 0.5),
        ];
        for (field, edit) in edits {
            assert_eq!(one(|s| edit(&mut s.tracks[1])), ("track_field", field, vec!["t2".to_owned()]));
        }
    }

    #[test]
    fn a_track_added_or_removed_is_reported() {
        let presence = ("track_field", "presence", vec!["t2".to_owned()]);
        assert_eq!(one(|s| drop(s.tracks.remove(1))), presence);
        let old = { let mut s = base(); s.tracks.remove(1); s };
        let changes = diff(&old, &base());
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0], ClipChange::TrackField { track_id: "t2".into(), field: "presence" });
    }

    #[test]
    fn every_sequence_member_no_planner_writes_is_reported() {
        let field = |field| ("sequence_field", field, vec![SEQ.to_owned()]);
        assert_eq!(one(|s| s.name = "renamed".into()), field("name"));
        assert_eq!(one(|s| s.frame_rate = SequenceFrameRate { ticks_per_frame: 20 }), field("frame_rate"));
        assert_eq!(one(|s| s.markers.reverse()), field("marker_order"));
        assert_eq!(one(|s| s.tracks.reverse()), field("track_order"));
    }

    #[test]
    fn the_head_slot_is_reported_like_the_tail() {
        let transition = TimelineTransition {
            schema_id: hsk_studio_folio::TransitionSchema::V1,
            transition_id: "tr".into(),
            transition_kind: "cross_dissolve".into(),
            duration: Ticks::new(2),
            alignment: hsk_studio_folio::TransitionAlignment::CenteredOnCut,
            center_offset: 0,
            parameters: Vec::new(),
            repeat_end_frame: false,
        };
        let old = base();
        let mut new = old.clone();
        new.tracks[0].clips[0].transitions.head = Some(transition);
        let changes = diff(&old, &new);
        assert!(matches!(changes.as_slice(), [ClipChange::ClipTransition { slot: TransitionSlot::Head, before: None, after: Some(_), .. }]), "{changes:?}");
        assert_eq!(changes[0].write_ids(), ["clip", "tr"]);
    }
}
