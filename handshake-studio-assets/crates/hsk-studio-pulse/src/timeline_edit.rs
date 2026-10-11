//! Sequence-time edit planners over Folio timeline members (STU-VID-026/027/029). A planner takes
//! a validated Folio `Snapshot`, runs the edit with pulse's tick/lane algebra, and returns a
//! proposal whose prospective document was validated by Folio itself (`validate_document`): pulse
//! does not re-implement document validation, and it never commits (STU-ARC-011).
//!
//! Semantics: a link group (shared non-null `link_group`) is selected and trimmed/slipped together,
//! one clip per track; razor, roll and slide refuse linked clips (`linked_edit_unsupported`).
//! Ripple shifts later content on the edited tracks and on every other sync-locked track; it is
//! refused when such a track is locked or the shift would collide. Video-track edits must move by
//! whole frames. Moving a source-out bound later needs a known source length (`SourceExtents`);
//! an unknown length refuses the extension rather than guessing.
use crate::error::{PulseError, Result};
use crate::range::{
    Grid, TickDelta, TickRange, offset, ripple_shift, ripple_trim_end, ripple_trim_start,
    roll_edit, slide, trim_end, trim_start,
};
use crate::rational::SpeedMath;
use crate::time::{checked_add, checked_sub};
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
    /// Splits one clip at `at`; the right half gets `new_right_clip_id`.
    Razor {
        clip_id: String,
        at: Ticks,
        new_right_clip_id: String,
    },
    /// Moves the edit between a clip and the clip that abuts it on the same track.
    Roll {
        left_clip_id: String,
        delta: TickDelta,
    },
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
}

#[derive(Default)]
struct Footprint {
    seen: BTreeSet<String>,
    ids: Vec<String>,
}
impl Footprint {
    fn add(&mut self, id: &str) {
        if self.seen.insert(id.to_owned()) {
            self.ids.push(id.to_owned());
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Head,
    Tail,
}

fn rate_of(seq: &StudioSequence) -> Result<FrameRate> {
    seq.frame_rate
        .accord()
        .ok_or(PulseError::UnsupportedFrameRate)
}

fn grid_of(track: &StudioTrack, rate: FrameRate) -> Grid {
    if track.kind == TrackKind::Video {
        Grid::Frames(rate)
    } else {
        Grid::Free
    }
}

fn lane_of(track: &StudioTrack) -> Result<Vec<TickRange>> {
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

fn refuse_linked(seq: &StudioSequence, clips: &[(usize, usize)]) -> Result<()> {
    if clips
        .iter()
        .any(|(t, c)| seq.tracks[*t].clips[*c].link_group.is_some())
    {
        return Err(PulseError::LinkedEditUnsupported);
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

fn check_extent(clip: &StudioClip, new_out: Ticks, ctx: &EditContext) -> Result<()> {
    match ctx.extents.extent(&clip.source_ref) {
        None => Err(PulseError::SourceExtentUnknown),
        Some(extent) if new_out.value() > extent.value() => Err(PulseError::PastSourceEnd),
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
fn shift_other_tracks(
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
    ctx: &EditContext,
    reads: &mut Footprint,
) -> Result<StudioSequence> {
    ctx.cancel.check()?;
    let rate = rate_of(seq)?;
    let (t, c) = locate(seq, clip_id)?;
    require_unlocked(seq, &[(t, c)], reads)?;
    refuse_linked(seq, &[(t, c)])?;
    if seq
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter())
        .any(|k| k.clip_id == new_id)
    {
        return Err(PulseError::DuplicateId);
    }
    let track = &seq.tracks[t];
    let clip = &track.clips[c];
    let whole = TickRange::from_start_duration(clip.timeline_start, clip.duration)?;
    let (left, right) = whole.split_at(at)?;
    grid_of(track, rate).check_delta(TickDelta::between(whole.start(), at))?;
    let span = clip.speed.span_for_duration(left.duration())?;
    // Forward plays source_in -> source_out, reversed plays source_out -> source_in; the cut
    // point is where the first half stops, and the second half continues from it.
    let ((left_in, left_out), (right_in, right_out)) = if clip.speed.is_reversed() {
        let cut = checked_sub(clip.source_out, span)?;
        ((cut, clip.source_out), (clip.source_in, cut))
    } else {
        let cut = checked_add(clip.source_in, span)?;
        ((clip.source_in, cut), (cut, clip.source_out))
    };
    let mut out = seq.clone();
    let mut second = clip.clone();
    second.clip_id = new_id.to_owned();
    second.source_in = right_in;
    second.source_out = right_out;
    second.timeline_start = right.start();
    second.duration = right.duration();
    let first = &mut out.tracks[t].clips[c];
    first.source_in = left_in;
    first.source_out = left_out;
    first.duration = left.duration();
    out.tracks[t].clips.insert(c + 1, second);
    Ok(out)
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
    let (t, c) = locate(seq, clip_id)?;
    require_unlocked(seq, &[(t, c)], reads)?;
    let track = &seq.tracks[t];
    let involved: Vec<(usize, usize)> = [c.checked_sub(1), Some(c), Some(c + 1)]
        .into_iter()
        .flatten()
        .filter(|i| *i < track.clips.len())
        .map(|i| (t, i))
        .collect();
    refuse_linked(seq, &involved)?;
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
    let mut out = seq.clone();
    write_lane(&mut out.tracks[t], &new_lane, &edges, ctx)?;
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
        } => razor(seq, clip_id, *at, new_right_clip_id, ctx, reads),
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
    let mut edited = apply_edit(old, edit, ctx, &mut reads)?;
    if edited == *old {
        return Err(PulseError::NoChange);
    }
    edited.duration = edited.content_end().ok_or(PulseError::Overflow)?;
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
    Ok(EditProposal {
        sequence_id: sequence_id.to_owned(),
        base_revision: expected_revision,
        target_revision,
        changes,
        read_footprint: reads.ids,
        write_footprint: writes.ids,
        snapshot: validated,
    })
}
