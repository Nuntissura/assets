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
    Budget, ClipSource, Code, Diagnostic, Resolver, Snapshot, StudioClip, StudioSequence,
    StudioTrack, TrackKind, validate_document,
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
}

impl ClipChange {
    pub fn clip_id(&self) -> &str {
        match self {
            Self::Retime { clip_id, .. } | Self::Remove { clip_id, .. } => clip_id,
            Self::Insert { clip, .. } => &clip.clip_id,
        }
    }
    pub fn track_id(&self) -> &str {
        match self {
            Self::Retime { track_id, .. }
            | Self::Insert { track_id, .. }
            | Self::Remove { track_id, .. } => track_id,
        }
    }
    pub fn before(&self) -> Option<ClipWindow> {
        match self {
            Self::Retime { before, .. } | Self::Remove { before, .. } => Some(*before),
            Self::Insert { .. } => None,
        }
    }
    pub fn after(&self) -> Option<ClipWindow> {
        match self {
            Self::Retime { after, .. } => Some(*after),
            Self::Insert { clip, .. } => Some(window(clip)),
            Self::Remove { .. } => None,
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
        self.changes.iter().find(|c| c.clip_id() == clip_id)
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
    /// Transitions this edit made unplayable (clips gone or no longer abutting, handle now
    /// short); removed from the sequence and listed, never silently lost (undo restores them).
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

fn apply_edit(
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

fn diff(old: &StudioSequence, new: &StudioSequence) -> Vec<ClipChange> {
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
    for track in &new.tracks {
        for clip in &track.clips {
            seen.insert(clip.clip_id.as_str());
            match before.get(clip.clip_id.as_str()) {
                Some((_, old_clip)) if window(old_clip) == window(clip) => {}
                Some((_, old_clip)) => changes.push(ClipChange::Retime {
                    clip_id: clip.clip_id.clone(),
                    track_id: track.track_id.clone(),
                    before: window(old_clip),
                    after: window(clip),
                }),
                None => changes.push(ClipChange::Insert {
                    track_id: track.track_id.clone(),
                    clip: Box::new(clip.clone()),
                }),
            }
        }
    }
    for track in &old.tracks {
        for clip in &track.clips {
            if !seen.contains(clip.clip_id.as_str()) {
                changes.push(ClipChange::Remove {
                    clip_id: clip.clip_id.clone(),
                    track_id: track.track_id.clone(),
                    before: window(clip),
                });
            }
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
        crate::transition::prune_transitions(&mut edited, ctx.extents, ctx.cancel)?
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
        writes.add(change.clip_id());
        if !matches!(change, ClipChange::Retime { .. }) {
            writes.add(change.track_id());
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
