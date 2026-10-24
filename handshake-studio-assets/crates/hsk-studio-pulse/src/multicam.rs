//! Multicam planners (lane SMB-PR-MULTICAM-1, STU-VID-033): create a multicam source sequence,
//! switch angles with or without a cut, toggle `audio_follows_video` / follow-nest / multicam mode,
//! renumber angles by selection order, capture a record pass, page and navigate the angle preview,
//! resolve the active angle at a tick, and flatten the cut list into ordinary clips. Multicam is a
//! clip mode (STU-VID-033): the clip's existing `ClipSource::Sequence` reference names the source
//! sequence and the Folio record set [`StudioMulticamRecords`] holds the mode (angles, booleans).
//!
//! Every planner returns ONE step ([`MulticamProposal`]): the prospective document (validated by
//! Folio, at revision + 1 when a sequence changed, else unchanged) plus the prospective record set
//! (validated by Folio against that document), and a [`MulticamInverse`] that
//! [`plan_multicam_revert`] turns back into the exact prior sequences and records (STU-VID-040 "A
//! multicam record pass is one step"; STU-VID-033 "`flatten` ... is one reversible history step").
//! Pulse holds no history and mints no ids: right-half and flattened clip ids come from the caller.
//!
//! Semantics (Studio readings, declared; Folio `multicam` module docs for the record readings):
//! - switch WITH a cut = Pulse's razor at the playhead (`TimelineEdit::Razor`, link groups split
//!   together) and the right halves take the angle; WITHOUT a cut = the whole clip takes it. The
//!   clip's link-group partners that are multicam clips switch with it; `audio_angle` follows only
//!   where `audio_follows_video` is on;
//! - a record pass is an ordered list of `(tick, angle)` events inside one clip: an event at the
//!   clip's first tick switches without a cut, every later event cuts; the whole pass is one step;
//! - flatten replaces each multicam clip by the clips of its angle track (video: `active_angle`;
//!   audio: the paired audio track of `audio_angle`) cut to the clip's source window, mapped onto
//!   the clip's place in the timeline; gaps in the angle track stay gaps; pieces are unlinked; the
//!   clip's head transition moves to a first piece that starts with the clip,
//!   its tail to a last piece that ends with it, anything else that no longer plays is dropped and
//!   listed (Pulse's transition pruning);
//! - clip markers through flatten (lane SMB-PR-MULTICAM-2; STU-VID-034 "A CLIP marker travels with
//!   the clip"; STU-VID-033 states no marker mapping, so this is a declared Studio reading): each
//!   marker of a flattened multicam clip (SOURCE ticks = source-sequence ticks) moves to the piece
//!   whose source-sequence span contains its start, converted to that piece's media ticks through the
//!   angle clip's speed (exact, else `multicam_speed_inexact`); id, name, comment, colour, duration
//!   (mapped the same way) and role are kept. A marker never becomes a SEQUENCE marker (STU-VID-034
//!   scopes differ): one in a gap of the angle track is refused `multicam_marker_in_gap`, a span
//!   marker that runs past its piece's end is refused `multicam_marker_spans_pieces`, and one outside
//!   the clip's source window is refused `multicam_marker_outside_window`;
//! - angle sync on creation: caller offsets or in points; timecode and audio-waveform sync are not
//!   specified by STU-VID-033 and are refused `multicam_sync_unsupported`;
//! - speed (lane SMB-PR-MULTICAM-2; STU-VID-031 "`speed` is a rational multiplier"; STU-VID-033
//!   states no speed mapping for angles, so this is a declared Studio reading): a multicam clip at
//!   forward speed `s` reads its source sequence `s` source ticks per timeline tick, an angle clip at
//!   forward speed `a` reads its media `a` media ticks per source-sequence tick. Flatten keeps each
//!   piece's media window (the angle clip's window cut to the overlap, mapped through `a`), maps its
//!   timeline span through `s`, and gives it the exact reduced product `s * a`; any derived tick that
//!   is not a whole number is refused `multicam_speed_inexact`. Reverse (negative) speed on the
//!   multicam clip or an angle clip it reads is refused `multicam_reverse_unsupported` (declared
//!   reading). [`resolve_active_angle`] uses the same mapping and FLOORS a fractional source position
//!   (a slowed clip holds a source tick for several timeline ticks; declared reading).
//!
//! Bounds (V4-01/02): record passes hold at most [`MAX_RECORD_PASS_EVENTS`] events, flatten at most
//! [`MAX_FLATTEN_PIECES`] pieces and [`MAX_FLATTEN_MARKERS`] carried markers (running total checked
//! before each clip is worked; each marker finds its piece by binary search), preview pages at most
//! [`MAX_PREVIEW_PAGE_SIZE`] angles; every count
//! is checked before work, every tick sum is checked, the cancel token is polled per event / clip.
//! Premiere Pro behaviour (angle numbering default, audio-follows-video default, cut placement,
//! flatten of speed-changed angles, marker placement after flatten, reverse-speed multicam) is
//! UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use crate::error::PulseError;
use crate::snap::{adjacent_edit, snap_targets};
use crate::timeline_edit::{EditContext, LinkedRazor, TimelineEdit, plan_timeline_edit};
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{
    ClipSource, ClipTransitions, Code, Diagnostic, MAX_MULTICAM_ANGLES, MAX_MULTICAM_CLIPS,
    MulticamSelectionOrder, SequenceFrameRate, SequenceSchema, Snapshot, Speed, StudioClip,
    StudioDocument, StudioMulticamClip, StudioMulticamRecords, StudioMulticamSequence, StudioSequence,
    StudioTrack, TrackKind, TrackSchema, multicam_angle_order, multicam_paired_audio_track,
    validate_document, validate_multicam_records,
};
use std::collections::BTreeSet;

/// Most events one record pass captures.
pub const MAX_RECORD_PASS_EVENTS: usize = 4096;
/// Most ordinary clips one flatten produces.
pub const MAX_FLATTEN_PIECES: usize = 65_536;
/// Most clip markers one flatten carries to its pieces.
pub const MAX_FLATTEN_MARKERS: usize = 65_536;
/// Most angles one preview page shows.
pub const MAX_PREVIEW_PAGE_SIZE: u32 = 64;

/// STU-VID-041 command ids carried in the proposal receipts (bindings belong to the GUI).
pub const CMD_CREATE_MULTICAM: &str = "cmd.clip.create.multicam";
pub const CMD_MULTICAM_FLATTEN: &str = "cmd.clip.multicam.flatten";
pub const CMD_MULTICAM_TOGGLE: &str = "cmd.clip.multicam.toggle";
pub const CMD_MULTICAM_TOGGLE_RECORD: &str = "cmd.multicam.toggle.record";
pub const CMD_MULTICAM_AUDIO_FOLLOWS_VIDEO: &str = "cmd.multicam.audio.follows.video";
pub const CMD_MULTICAM_FOLLOWS_NEST: &str = "cmd.multicam.follows.nest.setting";
pub const CMD_MULTICAM_SELECTION_TOP_DOWN: &str = "cmd.multicam.selection.top.down";

/// `cmd.multicam.choose.camera.{n}` (switch with a cut) or `cmd.multicam.choosenocut.camera{n}`.
pub fn multicam_camera_command(angle: u32, cut: bool) -> String {
    if cut {
        format!("cmd.multicam.choose.camera.{angle}")
    } else {
        format!("cmd.multicam.choosenocut.camera{angle}")
    }
}

/// Typed multicam refusal: pulse's own vocabulary plus the multicam-specific codes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MulticamError {
    Pulse(PulseError),
    /// The record set (input or prospective) failed Folio validation.
    Records(Diagnostic),
    SyncUnsupported,
    NotMulticam,
    AngleOutOfRange,
    EventOutsideClip,
    EventsUnordered,
    TooManyEvents,
    /// A speed mapping derives a tick that is not a whole number.
    SpeedInexact,
    /// A reverse (negative) speed on a multicam clip or an angle clip it reads.
    ReverseUnsupported,
    /// A clip marker starts in a gap of the angle track (no piece holds it).
    MarkerInGap,
    /// A span marker runs past the end of the piece that holds its start.
    MarkerSpansPieces,
    /// A clip marker starts outside the clip's source window.
    MarkerOutsideWindow,
}
impl MulticamError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Pulse(e) => e.code(),
            Self::Records(_) => "multicam_records_rejected",
            Self::SyncUnsupported => "multicam_sync_unsupported",
            Self::NotMulticam => "not_multicam",
            Self::AngleOutOfRange => "angle_out_of_range",
            Self::EventOutsideClip => "event_outside_clip",
            Self::EventsUnordered => "events_unordered",
            Self::TooManyEvents => "too_many_events",
            Self::SpeedInexact => "multicam_speed_inexact",
            Self::ReverseUnsupported => "multicam_reverse_unsupported",
            Self::MarkerInGap => "multicam_marker_in_gap",
            Self::MarkerSpansPieces => "multicam_marker_spans_pieces",
            Self::MarkerOutsideWindow => "multicam_marker_outside_window",
        }
    }
}
impl From<PulseError> for MulticamError {
    fn from(e: PulseError) -> Self {
        Self::Pulse(e)
    }
}
impl std::fmt::Display for MulticamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for MulticamError {}
pub type MulticamResult<T> = std::result::Result<T, MulticamError>;

fn folio(d: Diagnostic) -> MulticamError {
    match d.code {
        Code::Canceled => MulticamError::Pulse(PulseError::Canceled),
        Code::BudgetExceeded => MulticamError::Pulse(PulseError::LimitExceeded),
        _ => MulticamError::Pulse(PulseError::FolioRejected),
    }
}
fn records_err(d: Diagnostic) -> MulticamError {
    if d.code == Code::Canceled { MulticamError::Pulse(PulseError::Canceled) } else { MulticamError::Records(d) }
}
fn add(a: u64, b: u64) -> MulticamResult<u64> {
    a.checked_add(b).ok_or(MulticamError::Pulse(PulseError::Overflow))
}
fn sub(a: u64, b: u64) -> MulticamResult<u64> {
    a.checked_sub(b).ok_or(MulticamError::Pulse(PulseError::Underflow))
}

/// The reversible half of one multicam step: the document sequences and record set as the step
/// left them (`expect`) and as they were (`restore`); `sequences` is `None` when only records changed.
#[derive(Clone, Debug, PartialEq)]
pub struct MulticamInverse {
    sequences: Option<(Vec<StudioSequence>, Vec<StudioSequence>)>,
    records_expect: StudioMulticamRecords,
    records_restore: StudioMulticamRecords,
}
impl MulticamInverse {
    /// Records the step left (the revert precondition).
    pub fn records_expect(&self) -> &StudioMulticamRecords {
        &self.records_expect
    }
    /// Records the revert restores.
    pub fn records_restore(&self) -> &StudioMulticamRecords {
        &self.records_restore
    }
    /// Sequences the revert restores (`None` = the step changed no sequence).
    pub fn sequences_restore(&self) -> Option<&[StudioSequence]> {
        self.sequences.as_ref().map(|(_, r)| r.as_slice())
    }
}

/// One multicam step against one base revision (module docs).
#[derive(Clone, Debug)]
pub struct MulticamProposal {
    command_id: String,
    base_revision: u64,
    snapshot: Snapshot,
    records: StudioMulticamRecords,
    inverse: MulticamInverse,
    splits: Vec<(String, String)>,
    dropped_transitions: Vec<String>,
}
impl MulticamProposal {
    /// STU-VID-041 command id of the step.
    pub fn command_id(&self) -> &str {
        &self.command_id
    }
    pub fn base_revision(&self) -> u64 {
        self.base_revision
    }
    /// `base_revision + 1` when a sequence changed, else `base_revision`.
    pub fn target_revision(&self) -> u64 {
        self.snapshot.document().revision
    }
    /// The prospective document, validated by Folio.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    /// The prospective record set, validated by Folio against [`Self::snapshot`].
    pub fn records(&self) -> &StudioMulticamRecords {
        &self.records
    }
    /// Undo of this step for [`plan_multicam_revert`].
    pub fn inverse(&self) -> &MulticamInverse {
        &self.inverse
    }
    /// `(origin clip id, new clip id)` for every clip the step cut in two.
    pub fn splits(&self) -> &[(String, String)] {
        &self.splits
    }
    /// Transitions the step removed because they no longer play (undo restores them).
    pub fn dropped_transitions(&self) -> &[String] {
        &self.dropped_transitions
    }
    /// `(sequence id, before, after)` when exactly one existing sequence changed: the session's
    /// one-step `TimelineStep` (expect `before`, write `after`) commits it.
    pub fn sequence_change(&self) -> Option<(&str, &StudioSequence, &StudioSequence)> {
        let (after, before) = self.inverse.sequences.as_ref()?;
        if after.len() != before.len() {
            return None;
        }
        let mut changed = after.iter().zip(before).filter(|(a, b)| a != b);
        let (a, b) = changed.next()?;
        (changed.next().is_none() && a.sequence_id == b.sequence_id).then_some((a.sequence_id.as_str(), b, a))
    }
}

/// Stale-revision gate and validation of the caller's record set against the live document.
fn start(snapshot: &Snapshot, expected_revision: u64, records: &StudioMulticamRecords, ctx: &EditContext) -> MulticamResult<()> {
    ctx.cancel.check().map_err(PulseError::from)?;
    if snapshot.document().revision != expected_revision {
        return Err(PulseError::StaleRevision.into());
    }
    validate_multicam_records(records, snapshot.document(), ctx.budget, ctx.cancel).map_err(records_err)
}

/// Builds the one-step proposal: prospective sequences (revision + 1) and records, both validated.
#[allow(clippy::too_many_arguments)]
fn finish(
    snapshot: &Snapshot,
    expected_revision: u64,
    before: &StudioMulticamRecords,
    sequences: Option<Vec<StudioSequence>>,
    records: StudioMulticamRecords,
    command_id: String,
    splits: Vec<(String, String)>,
    dropped_transitions: Vec<String>,
    ctx: &EditContext,
) -> MulticamResult<MulticamProposal> {
    ctx.cancel.check().map_err(PulseError::from)?;
    let document = snapshot.document();
    let (prospective, sequences) = match sequences {
        Some(mut sequences) => {
            for s in &mut sequences {
                s.duration = s.content_end().ok_or(PulseError::Overflow)?;
            }
            if sequences == document.sequences {
                (snapshot.clone(), None)
            } else {
                let mut next: StudioDocument = document.clone();
                next.sequences = sequences.clone();
                next.revision = add(expected_revision, 1)?;
                let validated = validate_document(next, ctx.budget, ctx.cancel, ctx.resolver).map_err(folio)?;
                (validated, Some((sequences, document.sequences.clone())))
            }
        }
        None => (snapshot.clone(), None),
    };
    if sequences.is_none() && records == *before {
        return Err(PulseError::NoChange.into());
    }
    validate_multicam_records(&records, prospective.document(), ctx.budget, ctx.cancel).map_err(records_err)?;
    Ok(MulticamProposal {
        command_id,
        base_revision: expected_revision,
        snapshot: prospective,
        inverse: MulticamInverse { sequences, records_expect: records.clone(), records_restore: before.clone() },
        records,
        splits,
        dropped_transitions,
    })
}

/// Undo or redo of one applied multicam step: restores the inverse's sequences and records only while
/// the document still holds the sequences and the caller the records the step left (else
/// `revert_mismatch`). The result is an ordinary one-step proposal; its own inverse is the redo.
pub fn plan_multicam_revert(
    snapshot: &Snapshot,
    expected_revision: u64,
    records: &StudioMulticamRecords,
    inverse: &MulticamInverse,
    ctx: &EditContext,
) -> MulticamResult<MulticamProposal> {
    start(snapshot, expected_revision, records, ctx)?;
    if *records != inverse.records_expect {
        return Err(PulseError::RevertMismatch.into());
    }
    let sequences = match &inverse.sequences {
        Some((expect, restore)) => {
            if snapshot.document().sequences != *expect {
                return Err(PulseError::RevertMismatch.into());
            }
            Some(restore.clone())
        }
        None => None,
    };
    finish(
        snapshot,
        expected_revision,
        records,
        sequences,
        inverse.records_restore.clone(),
        "cmd.multicam.revert".to_owned(),
        Vec::new(),
        Vec::new(),
        ctx,
    )
}

/// `(sequence index, track index, clip index)` of `clip_id`.
fn locate(document: &StudioDocument, clip_id: &str) -> MulticamResult<(usize, usize, usize)> {
    document
        .sequences
        .iter()
        .enumerate()
        .find_map(|(s, seq)| {
            seq.tracks.iter().enumerate().find_map(|(t, track)| {
                track.clips.iter().position(|c| c.clip_id == clip_id).map(|c| (s, t, c))
            })
        })
        .ok_or(MulticamError::Pulse(PulseError::UnknownClip))
}

/// Angle count of the declared source sequence `clip` references (`None` = not a multicam nest).
fn angle_count(records: &StudioMulticamRecords, clip: &StudioClip) -> Option<usize> {
    let ClipSource::Sequence { sequence_id } = &clip.source_ref else {
        return None;
    };
    records.multicam_sequence(sequence_id).map(|m| m.angle_order.len())
}

/// The clip plus its link-group partners (same sequence) that carry an enabled multicam mode; the
/// primary itself must carry one (`not_multicam`).
fn members(seq: &StudioSequence, clip_id: &str, records: &StudioMulticamRecords) -> MulticamResult<Vec<String>> {
    if !records.clip(clip_id).is_some_and(|m| m.enabled) {
        return Err(MulticamError::NotMulticam);
    }
    let mut out = vec![clip_id.to_owned()];
    let group = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.clip_id == clip_id).and_then(|c| c.link_group.clone());
    if let Some(group) = group {
        for c in seq.tracks.iter().flat_map(|t| t.clips.iter()) {
            if c.clip_id != clip_id && c.link_group.as_deref() == Some(group.as_str()) && records.clip(&c.clip_id).is_some_and(|m| m.enabled) {
                out.push(c.clip_id.clone());
            }
        }
    }
    Ok(out)
}

fn check_angle(records: &StudioMulticamRecords, document: &StudioDocument, clip_ids: &[String], angle: u32) -> MulticamResult<()> {
    for id in clip_ids {
        let (s, t, c) = locate(document, id)?;
        let count = angle_count(records, &document.sequences[s].tracks[t].clips[c]).ok_or(MulticamError::NotMulticam)?;
        if !usize::try_from(angle).is_ok_and(|a| (1..=count).contains(&a)) {
            return Err(MulticamError::AngleOutOfRange);
        }
    }
    Ok(())
}

fn set_angle(records: &mut StudioMulticamRecords, clip_ids: &[String], angle: u32) {
    for id in clip_ids {
        if let Some(m) = records.clip_mut(id) {
            m.active_angle = angle;
            if m.audio_follows_video {
                m.audio_angle = angle;
            }
        }
    }
}

/// Fresh ids for a switch with a cut: the right half of the primary and, for a linked clip, the
/// right halves of its partners and their new link group (Pulse's razor contract).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MulticamCutIds {
    pub new_right_clip_id: String,
    pub linked: Option<LinkedRazor>,
}

/// One angle switch (STU-VID-033): with `cut_at` the clip is cut at the playhead and the later part
/// takes the angle; without it the whole clip does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitchAngle {
    pub clip_id: String,
    /// 1-based angle.
    pub angle: u32,
    pub cut_at: Option<Ticks>,
    pub cut_ids: Option<MulticamCutIds>,
}

#[allow(clippy::too_many_arguments)]
/// One razor of `primary` at `at` on the intermediate `snapshot`; returns the razored snapshot and
/// records with the right halves' modes copied from their left halves.
fn cut(
    snapshot: &Snapshot,
    records: &mut StudioMulticamRecords,
    primary: &str,
    at: Ticks,
    ids: &MulticamCutIds,
    ctx: &EditContext,
    splits: &mut Vec<(String, String)>,
    dropped: &mut Vec<String>,
) -> MulticamResult<Snapshot> {
    let document = snapshot.document();
    let (s, _, _) = locate(document, primary)?;
    let edit = TimelineEdit::Razor {
        clip_id: primary.to_owned(),
        at,
        new_right_clip_id: ids.new_right_clip_id.clone(),
        linked: ids.linked.clone(),
    };
    let proposal = plan_timeline_edit(snapshot, &document.sequences[s].sequence_id, document.revision, &edit, ctx)?;
    for (origin, fresh) in proposal.splits() {
        if let Some(mut m) = records.clip(origin).cloned() {
            m.clip_id = fresh.clone();
            records.clips.push(m);
        }
        splits.push((origin.clone(), fresh.clone()));
    }
    dropped.extend(proposal.dropped_transitions().iter().cloned());
    Ok(proposal.snapshot().clone())
}

/// Plans one angle switch (module docs). Command id [`multicam_camera_command`].
pub fn plan_multicam_switch(
    snapshot: &Snapshot,
    expected_revision: u64,
    records: &StudioMulticamRecords,
    switch: &SwitchAngle,
    ctx: &EditContext,
) -> MulticamResult<MulticamProposal> {
    start(snapshot, expected_revision, records, ctx)?;
    let document = snapshot.document();
    let (s, _, _) = locate(document, &switch.clip_id)?;
    let group = members(&document.sequences[s], &switch.clip_id, records)?;
    check_angle(records, document, &group, switch.angle)?;
    let mut next = records.clone();
    let mut splits = Vec::new();
    let mut dropped = Vec::new();
    let command = multicam_camera_command(switch.angle, switch.cut_at.is_some());
    match (switch.cut_at, &switch.cut_ids) {
        (None, None) => {
            set_angle(&mut next, &group, switch.angle);
            finish(snapshot, expected_revision, records, None, next, command, splits, dropped, ctx)
        }
        (Some(at), Some(ids)) => {
            let cut_snapshot = cut(snapshot, &mut next, &switch.clip_id, at, ids, ctx, &mut splits, &mut dropped)?;
            let right: Vec<String> =
                splits.iter().filter(|(origin, _)| group.contains(origin)).map(|(_, fresh)| fresh.clone()).collect();
            set_angle(&mut next, &right, switch.angle);
            let sequences = cut_snapshot.document().sequences.clone();
            finish(snapshot, expected_revision, records, Some(sequences), next, command, splits, dropped, ctx)
        }
        (Some(_), None) => Err(PulseError::InsufficientIds.into()),
        (None, Some(_)) => Err(PulseError::InvalidInput.into()),
    }
}

fn toggle(
    snapshot: &Snapshot,
    expected_revision: u64,
    records: &StudioMulticamRecords,
    clip_id: &str,
    command: &str,
    ctx: &EditContext,
    change: impl Fn(&mut StudioMulticamClip),
) -> MulticamResult<MulticamProposal> {
    start(snapshot, expected_revision, records, ctx)?;
    let document = snapshot.document();
    let (s, _, _) = locate(document, clip_id)?;
    let group = members(&document.sequences[s], clip_id, records)?;
    let mut next = records.clone();
    for id in &group {
        if let Some(m) = next.clip_mut(id) {
            change(m);
        }
    }
    finish(snapshot, expected_revision, records, None, next, command.to_owned(), Vec::new(), Vec::new(), ctx)
}

/// Toggles `audio_follows_video` on the clip and its multicam link partners
/// (`cmd.multicam.audio.follows.video`). Turning it on aligns the audio angle with the active angle
/// (declared reading: from then on a switch moves both).
pub fn plan_multicam_toggle_audio_follows_video(
    snapshot: &Snapshot,
    expected_revision: u64,
    records: &StudioMulticamRecords,
    clip_id: &str,
    ctx: &EditContext,
) -> MulticamResult<MulticamProposal> {
    let on = !records.clip(clip_id).is_some_and(|m| m.audio_follows_video);
    toggle(snapshot, expected_revision, records, clip_id, CMD_MULTICAM_AUDIO_FOLLOWS_VIDEO, ctx, |m| {
        m.audio_follows_video = on;
        if on {
            m.audio_angle = m.active_angle;
        }
    })
}

/// Toggles the STU-VID-030a follow-nest-setting boolean (`cmd.multicam.follows.nest.setting`).
pub fn plan_multicam_toggle_follows_nest(
    snapshot: &Snapshot,
    expected_revision: u64,
    records: &StudioMulticamRecords,
    clip_id: &str,
    ctx: &EditContext,
) -> MulticamResult<MulticamProposal> {
    let on = !records.clip(clip_id).is_some_and(|m| m.follows_nest_setting);
    toggle(snapshot, expected_revision, records, clip_id, CMD_MULTICAM_FOLLOWS_NEST, ctx, |m| m.follows_nest_setting = on)
}

/// Toggles multicam mode on a clip that nests a declared multicam source (`cmd.clip.multicam.toggle`
/// "Enable"), with its link partners that nest one. A clip without a mode gains one at angle 1 with
/// `audio_follows_video` on and follow-nest off (declared defaults; the spec states none).
pub fn plan_multicam_toggle_enabled(
    snapshot: &Snapshot,
    expected_revision: u64,
    records: &StudioMulticamRecords,
    clip_id: &str,
    ctx: &EditContext,
) -> MulticamResult<MulticamProposal> {
    start(snapshot, expected_revision, records, ctx)?;
    let document = snapshot.document();
    let (s, t, c) = locate(document, clip_id)?;
    let seq = &document.sequences[s];
    let primary = &seq.tracks[t].clips[c];
    angle_count(records, primary).ok_or(MulticamError::NotMulticam)?;
    let on = !records.clip(clip_id).is_some_and(|m| m.enabled);
    let mut next = records.clone();
    for clip in seq.tracks.iter().flat_map(|k| k.clips.iter()) {
        let partner = clip.clip_id == clip_id
            || (primary.link_group.is_some() && clip.link_group == primary.link_group && angle_count(records, clip).is_some());
        if !partner {
            continue;
        }
        match next.clip_mut(&clip.clip_id) {
            Some(m) => m.enabled = on,
            None if on => next.clips.push(StudioMulticamClip {
                clip_id: clip.clip_id.clone(),
                active_angle: 1,
                audio_angle: 1,
                audio_follows_video: true,
                follows_nest_setting: false,
                enabled: true,
            }),
            None => {}
        }
    }
    finish(snapshot, expected_revision, records, None, next, CMD_MULTICAM_TOGGLE.to_owned(), Vec::new(), Vec::new(), ctx)
}

/// Renumbers the angles of source sequence `sequence_id` by `order` (STU-VID-033 selection order;
/// command `cmd.multicam.selection.top.down`). Every clip keeps the TRACK it shows and plays: its
/// angle numbers are remapped to the new numbering.
pub fn plan_multicam_angle_numbering(
    snapshot: &Snapshot,
    expected_revision: u64,
    records: &StudioMulticamRecords,
    sequence_id: &str,
    order: MulticamSelectionOrder,
    ctx: &EditContext,
) -> MulticamResult<MulticamProposal> {
    start(snapshot, expected_revision, records, ctx)?;
    let document = snapshot.document();
    let seq = document.sequences.iter().find(|s| s.sequence_id == sequence_id).ok_or(PulseError::UnknownSequence)?;
    let old = records.multicam_sequence(sequence_id).ok_or(MulticamError::NotMulticam)?.angle_order.clone();
    let new = multicam_angle_order(seq, order);
    let remap = |angle: u32| -> MulticamResult<u32> {
        let index = usize::try_from(angle).ok().and_then(|a| a.checked_sub(1)).ok_or(MulticamError::AngleOutOfRange)?;
        let track = old.get(index).ok_or(MulticamError::AngleOutOfRange)?;
        let position = new.iter().position(|t| t == track).ok_or(MulticamError::AngleOutOfRange)?;
        u32::try_from(position).ok().and_then(|p| p.checked_add(1)).ok_or(MulticamError::Pulse(PulseError::Overflow))
    };
    let mut next = records.clone();
    for m in &mut next.clips {
        ctx.cancel.check().map_err(PulseError::from)?;
        let (s, t, c) = locate(document, &m.clip_id)?;
        let nests_this = matches!(&document.sequences[s].tracks[t].clips[c].source_ref,
            ClipSource::Sequence { sequence_id: n } if n == sequence_id);
        if nests_this {
            m.active_angle = remap(m.active_angle)?;
            m.audio_angle = remap(m.audio_angle)?;
        }
    }
    if let Some(decl) = next.multicam_sequences.iter_mut().find(|m| m.sequence_id == sequence_id) {
        decl.selection_order = order;
        decl.angle_order = new;
    }
    finish(snapshot, expected_revision, records, None, next, CMD_MULTICAM_SELECTION_TOP_DOWN.to_owned(), Vec::new(), Vec::new(), ctx)
}

/// How the angles of a new multicam source are aligned (STU-VID-033 does not specify a method).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MulticamSync {
    /// Each angle starts at its caller-supplied offset in the source sequence.
    CallerOffsets,
    /// Every angle starts at tick 0 (its in point); offsets must all be 0.
    InPoints,
    /// Refused `multicam_sync_unsupported`.
    Timecode,
    /// Refused `multicam_sync_unsupported`.
    AudioWaveform,
}

/// One angle of a new multicam source: its picture clip, optional sound clip, and where it starts.
#[derive(Clone, Debug, PartialEq)]
pub struct MulticamAngleInput {
    pub video: StudioClip,
    pub audio: Option<StudioClip>,
    pub offset: Ticks,
}

/// `cmd.clip.create.multicam` "Create Multi-Camera Source Sequence...".
#[derive(Clone, Debug, PartialEq)]
pub struct MulticamSourceRequest {
    pub sequence_id: String,
    pub name: String,
    pub frame_rate: SequenceFrameRate,
    pub sync: MulticamSync,
    pub selection_order: MulticamSelectionOrder,
    /// In source order: angle input i goes to video track index i and audio track index i.
    pub angles: Vec<MulticamAngleInput>,
    /// One fresh video track id and one fresh audio track id per angle.
    pub video_track_ids: Vec<String>,
    pub audio_track_ids: Vec<String>,
}

fn new_track(track_id: &str, kind: TrackKind, index: usize, name: String, clips: Vec<StudioClip>) -> MulticamResult<StudioTrack> {
    Ok(StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: track_id.to_owned(),
        kind,
        index: u32::try_from(index).map_err(|_| PulseError::LimitExceeded)?,
        name,
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        clips,
        volume: 1.0,
        pan: 0.0,
    })
}

/// Creates a multicam source sequence from the angle clips (one video track and one audio track
/// per angle, aligned by `sync`) and declares it in the records with the requested numbering.
pub fn plan_create_multicam_source(
    snapshot: &Snapshot,
    expected_revision: u64,
    records: &StudioMulticamRecords,
    request: &MulticamSourceRequest,
    ctx: &EditContext,
) -> MulticamResult<MulticamProposal> {
    start(snapshot, expected_revision, records, ctx)?;
    if matches!(request.sync, MulticamSync::Timecode | MulticamSync::AudioWaveform) {
        return Err(MulticamError::SyncUnsupported);
    }
    let n = request.angles.len();
    if n == 0 {
        return Err(PulseError::InvalidInput.into());
    }
    if n > MAX_MULTICAM_ANGLES {
        return Err(PulseError::LimitExceeded.into());
    }
    if request.video_track_ids.len() != n || request.audio_track_ids.len() != n {
        return Err(PulseError::InsufficientIds.into());
    }
    let document = snapshot.document();
    if document.sequences.iter().any(|s| s.sequence_id == request.sequence_id) {
        return Err(PulseError::DuplicateId.into());
    }
    let frame = request.frame_rate.ticks_per_frame;
    let mut video = Vec::with_capacity(n);
    let mut audio = Vec::with_capacity(n);
    for (i, angle) in request.angles.iter().enumerate() {
        ctx.cancel.check().map_err(PulseError::from)?;
        let offset = angle.offset.value();
        if request.sync == MulticamSync::InPoints && offset != 0 {
            return Err(PulseError::InvalidInput.into());
        }
        if frame == 0 || offset.checked_rem(frame) != Some(0) {
            return Err(PulseError::NotFrameAligned.into());
        }
        let place = |c: &StudioClip| {
            let mut c = c.clone();
            c.timeline_start = Ticks::new(offset);
            c
        };
        let number = i.checked_add(1).ok_or(PulseError::Overflow)?;
        video.push(new_track(&request.video_track_ids[i], TrackKind::Video, i, format!("Camera {number}"), vec![place(&angle.video)])?);
        let sound = angle.audio.as_ref().map(place).into_iter().collect();
        audio.push(new_track(&request.audio_track_ids[i], TrackKind::Audio, i, format!("Camera {number} audio"), sound)?);
    }
    video.extend(audio);
    let seq = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: request.sequence_id.clone(),
        name: request.name.clone(),
        frame_rate: request.frame_rate,
        duration: Ticks::new(0),
        tracks: video,
        markers: Vec::new(),
        work_area: None,
    };
    let mut next = records.clone();
    next.multicam_sequences.push(StudioMulticamSequence {
        sequence_id: request.sequence_id.clone(),
        selection_order: request.selection_order,
        angle_order: multicam_angle_order(&seq, request.selection_order),
    });
    let mut sequences = document.sequences.clone();
    sequences.push(seq);
    finish(snapshot, expected_revision, records, Some(sequences), next, CMD_CREATE_MULTICAM.to_owned(), Vec::new(), Vec::new(), ctx)
}

/// One live angle switch captured during playback: an event at the clip's first tick switches the
/// whole first segment (no ids); every later event cuts there and needs `cut_ids`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordEvent {
    pub at: Ticks,
    pub angle: u32,
    pub cut_ids: Option<MulticamCutIds>,
}

/// Plans one record pass over clip `clip_id` (STU-VID-040 "A multicam record pass is one step";
/// command `cmd.multicam.toggle.record`): the events, strictly increasing and inside the clip, become
/// one cut list in ONE step with ONE inverse.
pub fn plan_multicam_record_pass(
    snapshot: &Snapshot,
    expected_revision: u64,
    records: &StudioMulticamRecords,
    clip_id: &str,
    events: &[RecordEvent],
    ctx: &EditContext,
) -> MulticamResult<MulticamProposal> {
    if events.len() > MAX_RECORD_PASS_EVENTS {
        return Err(MulticamError::TooManyEvents);
    }
    start(snapshot, expected_revision, records, ctx)?;
    if events.is_empty() {
        return Err(PulseError::NoChange.into());
    }
    let document = snapshot.document();
    let (s, t, c) = locate(document, clip_id)?;
    let clip = &document.sequences[s].tracks[t].clips[c];
    members(&document.sequences[s], clip_id, records)?;
    let first = clip.timeline_start;
    let end = clip.timeline_end().ok_or(PulseError::Overflow)?;
    let mut last: Option<Ticks> = None;
    for e in events {
        if e.at < first || e.at >= end {
            return Err(MulticamError::EventOutsideClip);
        }
        if last.is_some_and(|l| e.at <= l) {
            return Err(MulticamError::EventsUnordered);
        }
        last = Some(e.at);
    }
    let mut current = snapshot.clone();
    let mut current_id = clip_id.to_owned();
    let mut next = records.clone();
    let mut splits = Vec::new();
    let mut dropped = Vec::new();
    for e in events {
        ctx.cancel.check().map_err(PulseError::from)?;
        let (cs, _, _) = locate(current.document(), &current_id)?;
        let group = members(&current.document().sequences[cs], &current_id, &next)?;
        check_angle(&next, current.document(), &group, e.angle)?;
        if e.at == first {
            if e.cut_ids.is_some() {
                return Err(PulseError::InvalidInput.into());
            }
            set_angle(&mut next, &group, e.angle);
            continue;
        }
        let ids = e.cut_ids.as_ref().ok_or(PulseError::InsufficientIds)?;
        let before = splits.len();
        current = cut(&current, &mut next, &current_id, e.at, ids, ctx, &mut splits, &mut dropped)?;
        let right: Vec<String> =
            splits[before..].iter().filter(|(origin, _)| group.contains(origin)).map(|(_, fresh)| fresh.clone()).collect();
        set_angle(&mut next, &right, e.angle);
        current_id = ids.new_right_clip_id.clone();
    }
    let sequences = current.document().sequences.clone();
    finish(snapshot, expected_revision, records, Some(sequences), next, CMD_MULTICAM_TOGGLE_RECORD.to_owned(), splits, dropped, ctx)
}

/// What a multicam clip shows and plays at one sequence tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAngle {
    pub angle: u32,
    pub audio_angle: u32,
    /// Video track of `angle` in the source sequence.
    pub angle_track_id: String,
    /// Paired audio track of `audio_angle` (`None` = a silent angle).
    pub audio_track_id: Option<String>,
    /// The tick in the source sequence.
    pub source_tick: Ticks,
    /// The angle-track clip under `source_tick` and the tick in ITS source (`None` = a gap).
    pub picture: Option<(String, Ticks)>,
    pub sound: Option<(String, Ticks)>,
}

/// A forward speed as `(|numerator|, denominator)`; reverse is `multicam_reverse_unsupported`.
fn forward(speed: &Speed) -> MulticamResult<(u128, u128)> {
    if speed.is_reversed() {
        return Err(MulticamError::ReverseUnsupported);
    }
    if speed.numerator == 0 || speed.denominator == 0 {
        return Err(PulseError::InvalidSpeed.into());
    }
    Ok((u128::from(speed.numerator.unsigned_abs()), u128::from(speed.denominator)))
}
/// `(denominator, numerator)`: the inverse ratio (source offset -> timeline offset).
fn inverse_ratio((num, den): (u128, u128)) -> (u128, u128) {
    (den, num)
}
/// `x * num / den` exactly (`multicam_speed_inexact` when not a whole number of ticks).
fn scale_exact(x: u64, (num, den): (u128, u128)) -> MulticamResult<u64> {
    let product = u128::from(x).checked_mul(num).ok_or(PulseError::Overflow)?;
    let q = product.checked_div(den).ok_or(PulseError::InvalidSpeed)?;
    if product.checked_rem(den) != Some(0) {
        return Err(MulticamError::SpeedInexact);
    }
    u64::try_from(q).map_err(|_| PulseError::Overflow.into())
}
/// `floor(x * num / den)`.
fn scale_floor(x: u64, (num, den): (u128, u128)) -> MulticamResult<u64> {
    let product = u128::from(x).checked_mul(num).ok_or(PulseError::Overflow)?;
    let q = product.checked_div(den).ok_or(PulseError::InvalidSpeed)?;
    u64::try_from(q).map_err(|_| PulseError::Overflow.into())
}
fn gcd128(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a.checked_rem(b).unwrap_or(0));
    }
    a
}
/// The exact reduced product of two forward speeds (`overflow` when it has no `Speed` spelling).
fn speed_product(s: (u128, u128), a: (u128, u128)) -> MulticamResult<Speed> {
    let num = s.0.checked_mul(a.0).ok_or(PulseError::Overflow)?;
    let den = s.1.checked_mul(a.1).ok_or(PulseError::Overflow)?;
    let g = gcd128(num, den);
    let num = num.checked_div(g).ok_or(PulseError::InvalidSpeed)?;
    let den = den.checked_div(g).ok_or(PulseError::InvalidSpeed)?;
    Ok(Speed {
        numerator: i64::try_from(num).map_err(|_| PulseError::Overflow)?,
        denominator: u64::try_from(den).map_err(|_| PulseError::Overflow)?,
    })
}

/// The clip of `track` under source-sequence tick `at` and the tick in its own source (the angle
/// clip's speed applied, floored; module docs).
fn under(track: Option<&StudioTrack>, at: u64) -> MulticamResult<Option<(String, Ticks)>> {
    let Some(track) = track else { return Ok(None) };
    for c in &track.clips {
        let start = c.timeline_start.value();
        let end = add(start, c.duration.value())?;
        if at >= start && at < end {
            let a = forward(&c.speed)?;
            return Ok(Some((c.clip_id.clone(), Ticks::new(add(c.source_in.value(), scale_floor(sub(at, start)?, a)?)?))));
        }
    }
    Ok(None)
}

/// Resolves which angle multicam clip `clip_id` shows and plays at sequence tick `tick` and the
/// source clips under it (the cut list read at one tick; program-monitor playback of the active angle
/// is not wired: pulse playback does not resolve nested sequences).
pub fn resolve_active_angle(
    snapshot: &Snapshot,
    records: &StudioMulticamRecords,
    clip_id: &str,
    tick: Ticks,
    ctx: &EditContext,
) -> MulticamResult<ResolvedAngle> {
    start(snapshot, snapshot.document().revision, records, ctx)?;
    let document = snapshot.document();
    let (s, t, c) = locate(document, clip_id)?;
    let clip = &document.sequences[s].tracks[t].clips[c];
    let mode = records.clip(clip_id).filter(|m| m.enabled).ok_or(MulticamError::NotMulticam)?;
    let s = forward(&clip.speed)?;
    let end = clip.timeline_end().ok_or(PulseError::Overflow)?;
    if tick < clip.timeline_start || tick >= end {
        return Err(PulseError::OutsideRange.into());
    }
    let ClipSource::Sequence { sequence_id } = &clip.source_ref else {
        return Err(MulticamError::NotMulticam);
    };
    let source = document.sequences.iter().find(|q| &q.sequence_id == sequence_id).ok_or(PulseError::UnknownSequence)?;
    let decl = records.multicam_sequence(sequence_id).ok_or(MulticamError::NotMulticam)?;
    let pick = |angle: u32| -> MulticamResult<&String> {
        let i = usize::try_from(angle).ok().and_then(|a| a.checked_sub(1)).ok_or(MulticamError::AngleOutOfRange)?;
        decl.angle_order.get(i).ok_or(MulticamError::AngleOutOfRange)
    };
    let angle_track_id = pick(mode.active_angle)?.clone();
    let audio_track_id = multicam_paired_audio_track(source, pick(mode.audio_angle)?).map(str::to_owned);
    let at = add(clip.source_in.value(), scale_floor(sub(tick.value(), clip.timeline_start.value())?, s)?)?;
    let find = |id: Option<&String>| id.and_then(|id| source.tracks.iter().find(|k| &k.track_id == id));
    Ok(ResolvedAngle {
        angle: mode.active_angle,
        audio_angle: mode.audio_angle,
        picture: under(find(Some(&angle_track_id)), at)?,
        sound: under(find(audio_track_id.as_ref()), at)?,
        angle_track_id,
        audio_track_id,
        source_tick: Ticks::new(at),
    })
}

#[allow(clippy::too_many_arguments)]
/// The ordinary clips that replace multicam clip `clip` (module docs), taking ids from `fresh`.
fn flatten_clip(
    clip: &StudioClip,
    kind: TrackKind,
    mode: &StudioMulticamClip,
    source: &StudioSequence,
    decl: &StudioMulticamSequence,
    fresh: &mut dyn Iterator<Item = &String>,
    taken: &mut BTreeSet<String>,
    dropped: &mut Vec<String>,
    cancel: &CancellationToken,
) -> MulticamResult<Vec<StudioClip>> {
    let s = forward(&clip.speed)?;
    let pick = |angle: u32| -> MulticamResult<&str> {
        let i = usize::try_from(angle).ok().and_then(|a| a.checked_sub(1)).ok_or(MulticamError::AngleOutOfRange)?;
        decl.angle_order.get(i).map(String::as_str).ok_or(MulticamError::AngleOutOfRange)
    };
    let track_id = match kind {
        TrackKind::Video => Some(pick(mode.active_angle)?),
        TrackKind::Audio => multicam_paired_audio_track(source, pick(mode.audio_angle)?),
        TrackKind::Caption | TrackKind::Submix => return Err(MulticamError::NotMulticam),
    };
    let (w0, w1) = (clip.source_in.value(), clip.source_out.value());
    let mut pieces = Vec::new();
    // Per piece: its source-sequence span and the angle clip's speed (marker mapping).
    let mut spans: Vec<(u64, u64, (u128, u128))> = Vec::new();
    if let Some(track) = track_id.and_then(|id| source.tracks.iter().find(|k| k.track_id == id)) {
        for c in &track.clips {
            let start = c.timeline_start.value();
            let end = add(start, c.duration.value())?;
            let (o0, o1) = (w0.max(start), w1.min(end));
            if o0 >= o1 {
                continue;
            }
            let a = forward(&c.speed)?;
            let id = fresh.next().ok_or(PulseError::InsufficientIds)?;
            if !taken.insert(id.clone()) {
                return Err(PulseError::DuplicateId.into());
            }
            let mut piece = c.clone();
            piece.clip_id = id.clone();
            piece.source_in = Ticks::new(add(c.source_in.value(), scale_exact(sub(o0, start)?, a)?)?);
            piece.source_out = Ticks::new(add(c.source_in.value(), scale_exact(sub(o1, start)?, a)?)?);
            piece.timeline_start = Ticks::new(add(clip.timeline_start.value(), scale_exact(sub(o0, w0)?, inverse_ratio(s))?)?);
            piece.duration = Ticks::new(scale_exact(sub(o1, o0)?, inverse_ratio(s))?);
            piece.speed = speed_product(s, a)?;
            spans.push((o0, o1, a));
            piece.enabled = clip.enabled && c.enabled;
            piece.link_group = None;
            piece.markers = Vec::new();
            piece.transitions = ClipTransitions::default();
            pieces.push(piece);
        }
    }
    carry_markers(clip, &spans, &mut pieces, cancel)?;
    let head = clip.transitions.head.clone();
    let tail = clip.transitions.tail.clone();
    match pieces.first_mut() {
        Some(p) if p.timeline_start == clip.timeline_start => p.transitions.head = head,
        _ => dropped.extend(head.map(|h| h.transition_id)),
    }
    let clip_end = clip.timeline_end().ok_or(PulseError::Overflow)?;
    match pieces.last_mut() {
        Some(p) if p.timeline_end() == Some(clip_end) => p.transitions.tail = tail,
        _ => dropped.extend(tail.map(|t| t.transition_id)),
    }
    Ok(pieces)
}

/// Moves every marker of multicam clip `clip` onto the piece that holds its start (module docs).
/// `spans` are the pieces' source-sequence spans in order (disjoint and increasing: they come from
/// the ordered, non-overlapping clips of one angle track).
fn carry_markers(
    clip: &StudioClip,
    spans: &[(u64, u64, (u128, u128))],
    pieces: &mut [StudioClip],
    cancel: &CancellationToken,
) -> MulticamResult<()> {
    let (w0, w1) = (clip.source_in.value(), clip.source_out.value());
    for m in &clip.markers {
        cancel.check().map_err(PulseError::from)?;
        let start = m.start.value();
        if start < w0 || start >= w1 {
            return Err(MulticamError::MarkerOutsideWindow);
        }
        let index = spans.partition_point(|(o0, _, _)| *o0 <= start).checked_sub(1).ok_or(MulticamError::MarkerInGap)?;
        let (o0, o1, a) = spans[index];
        if start >= o1 {
            return Err(MulticamError::MarkerInGap);
        }
        if add(start, m.duration.value())? > o1 {
            return Err(MulticamError::MarkerSpansPieces);
        }
        let piece = &mut pieces[index];
        let mut moved = m.clone();
        moved.start = Ticks::new(add(piece.source_in.value(), scale_exact(sub(start, o0)?, a)?)?);
        moved.duration = Ticks::new(scale_exact(m.duration.value(), a)?);
        piece.markers.push(moved);
    }
    for piece in pieces.iter_mut() {
        piece.markers.sort_by_key(|m| m.start);
    }
    Ok(())
}

/// Plans `flatten` (STU-VID-033, command `cmd.clip.multicam.flatten`): every listed multicam clip
/// becomes the ordinary clips of its current angle (module docs), its multicam mode is discarded,
/// and the whole flatten is one reversible step. Piece ids come from `fresh_clip_ids` in clip order,
/// then piece order.
pub fn plan_multicam_flatten(
    snapshot: &Snapshot,
    expected_revision: u64,
    records: &StudioMulticamRecords,
    clip_ids: &[String],
    fresh_clip_ids: &[String],
    ctx: &EditContext,
) -> MulticamResult<MulticamProposal> {
    if clip_ids.len() > MAX_MULTICAM_CLIPS || fresh_clip_ids.len() > MAX_FLATTEN_PIECES {
        return Err(PulseError::LimitExceeded.into());
    }
    start(snapshot, expected_revision, records, ctx)?;
    if clip_ids.is_empty() {
        return Err(PulseError::NoChange.into());
    }
    let document = snapshot.document();
    let mut taken: BTreeSet<String> = document
        .sequences
        .iter()
        .flat_map(|s| s.tracks.iter())
        .flat_map(|t| t.clips.iter())
        .map(|c| c.clip_id.clone())
        .collect();
    let mut sequences = document.sequences.clone();
    let mut fresh = fresh_clip_ids.iter();
    let mut dropped = Vec::new();
    let mut seen = BTreeSet::new();
    let mut next = records.clone();
    let mut touched = BTreeSet::new();
    let mut markers = 0usize;
    for clip_id in clip_ids {
        ctx.cancel.check().map_err(PulseError::from)?;
        if !seen.insert(clip_id.as_str()) {
            return Err(PulseError::DuplicateId.into());
        }
        let mode = records.clip(clip_id).filter(|m| m.enabled).ok_or(MulticamError::NotMulticam)?;
        let (s, t, _) = locate(document, clip_id)?;
        let position = sequences[s].tracks[t].clips.iter().position(|c| &c.clip_id == clip_id).ok_or(PulseError::UnknownClip)?;
        let clip = sequences[s].tracks[t].clips[position].clone();
        markers = markers.checked_add(clip.markers.len()).ok_or(PulseError::LimitExceeded)?;
        if markers > MAX_FLATTEN_MARKERS {
            return Err(PulseError::LimitExceeded.into());
        }
        let ClipSource::Sequence { sequence_id } = &clip.source_ref else {
            return Err(MulticamError::NotMulticam);
        };
        let source = document.sequences.iter().find(|q| &q.sequence_id == sequence_id).ok_or(PulseError::UnknownSequence)?;
        let decl = records.multicam_sequence(sequence_id).ok_or(MulticamError::NotMulticam)?;
        let kind = sequences[s].tracks[t].kind;
        let pieces = flatten_clip(&clip, kind, mode, source, decl, &mut fresh, &mut taken, &mut dropped, ctx.cancel)?;
        let track = &mut sequences[s].tracks[t];
        let after = position.checked_add(1).ok_or(PulseError::Overflow)?;
        let rest = track.clips.split_off(after);
        track.clips.truncate(position);
        track.clips.extend(pieces);
        track.clips.extend(rest);
        next.clips.retain(|m| &m.clip_id != clip_id);
        touched.insert(s);
    }
    for s in touched {
        dropped.extend(crate::transition::prune_transitions(&mut sequences[s], ctx.extents, ctx.cancel)?);
    }
    finish(snapshot, expected_revision, records, Some(sequences), next, CMD_MULTICAM_FLATTEN.to_owned(), Vec::new(), dropped, ctx)
}

/// Preview navigation commands (STU-VID-033 "first/last/next/previous page navigation, next/previous
/// angle, and next/previous edit navigation").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MulticamNav {
    PageFirst,
    PageLast,
    PageNext,
    PagePrevious,
    NextCamera,
    PreviousCamera,
    NextEdit,
    PreviousEdit,
}
impl MulticamNav {
    pub const fn command_id(self) -> &'static str {
        match self {
            Self::PageFirst => "cmd.multicam.page.first",
            Self::PageLast => "cmd.multicam.page.last",
            Self::PageNext => "cmd.multicam.page.next",
            Self::PagePrevious => "cmd.multicam.page.previous",
            Self::NextCamera => "cmd.multicam.next.camera",
            Self::PreviousCamera => "cmd.multicam.previous.camera",
            Self::NextEdit => "cmd.multicam.next.edit",
            Self::PreviousEdit => "cmd.multicam.prev.edit",
        }
    }
}

/// One page of the paged angle preview grid (0-based `page`). Navigation clamps at the ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MulticamPage {
    page_size: u32,
    page: u32,
    angle_count: u32,
}
impl MulticamPage {
    /// `invalid_input` for a page size outside `1..=MAX_PREVIEW_PAGE_SIZE` or an angle count outside
    /// `1..=MAX_MULTICAM_ANGLES`; `outside_range` for a page past the last.
    pub fn new(page_size: u32, page: u32, angle_count: u32) -> MulticamResult<Self> {
        let angles_ok = usize::try_from(angle_count).is_ok_and(|a| (1..=MAX_MULTICAM_ANGLES).contains(&a));
        if !(1..=MAX_PREVIEW_PAGE_SIZE).contains(&page_size) || !angles_ok {
            return Err(PulseError::InvalidInput.into());
        }
        let p = Self { page_size, page: 0, angle_count };
        if page >= p.page_count() {
            return Err(PulseError::OutsideRange.into());
        }
        Ok(Self { page, ..p })
    }
    /// The page that shows `angle` (1-based).
    pub fn containing(page_size: u32, angle: u32, angle_count: u32) -> MulticamResult<Self> {
        let p = Self::new(page_size, 0, angle_count)?;
        if !(1..=angle_count).contains(&angle) {
            return Err(MulticamError::AngleOutOfRange);
        }
        let page = angle.saturating_sub(1).checked_div(page_size).ok_or(PulseError::InvalidInput)?;
        Self::new(p.page_size, page, angle_count)
    }
    pub fn page(self) -> u32 {
        self.page
    }
    pub fn page_size(self) -> u32 {
        self.page_size
    }
    pub fn page_count(self) -> u32 {
        self.angle_count.div_ceil(self.page_size)
    }
    /// The 1-based angles this page shows.
    pub fn angles(self) -> std::ops::RangeInclusive<u32> {
        let first = self.page.saturating_mul(self.page_size).saturating_add(1);
        let last = self.page.saturating_add(1).saturating_mul(self.page_size).min(self.angle_count);
        first..=last
    }
    pub fn first(self) -> Self {
        Self { page: 0, ..self }
    }
    pub fn last(self) -> Self {
        Self { page: self.page_count().saturating_sub(1), ..self }
    }
    pub fn next(self) -> Self {
        Self { page: self.page.saturating_add(1).min(self.page_count().saturating_sub(1)), ..self }
    }
    pub fn previous(self) -> Self {
        Self { page: self.page.saturating_sub(1), ..self }
    }
}

/// Next (`forward`) or previous angle from `current`, clamped to `1..=angle_count` (declared: no wrap).
pub fn multicam_step_angle(current: u32, angle_count: u32, forward: bool) -> MulticamResult<u32> {
    if !(1..=angle_count).contains(&current) || !usize::try_from(angle_count).is_ok_and(|a| a <= MAX_MULTICAM_ANGLES) {
        return Err(MulticamError::AngleOutOfRange);
    }
    Ok(if forward { current.saturating_add(1).min(angle_count) } else { current.saturating_sub(1).max(1) })
}

/// The next or previous edit of `sequence` from `from` (`cmd.multicam.next.edit` / `prev.edit`):
/// Pulse's snap edit points through [`adjacent_edit`].
pub fn multicam_adjacent_edit(
    sequence: &StudioSequence,
    from: Ticks,
    forward: bool,
    cancel: &CancellationToken,
) -> MulticamResult<Option<Ticks>> {
    let targets = snap_targets(sequence, None, &[], cancel)?;
    Ok(adjacent_edit(&targets, from, forward))
}
