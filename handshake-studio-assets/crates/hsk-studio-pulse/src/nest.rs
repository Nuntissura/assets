//! Nesting and subsequences (lane SMB-PR-NEST-1, STU-VID-030 "A sequence may be a clip"): the Nest
//! and Make Subsequence planners and the playback resolution of nested sequence clips.
//!
//! **Planners.** [`plan_nest`] replaces a selection with one clip per track kind whose source is a
//! newly created sequence holding that selection; [`plan_make_subsequence`] creates the same kind of
//! sequence from a selection or from an in-to-out range and leaves the parent unchanged. Both return
//! ONE step ([`NestProposal`]): the prospective document at revision + 1 (validated by Folio's
//! `validate_document`, which also refuses a containment cycle; pulse re-implements no validation),
//! the [`SequenceChange`] list that turns the live document into it (Insert of the new sequence,
//! and for Nest the Replace of the parent) and the inverse list (STU-VID-040 one reversible step).
//! [`apply_sequence_changes`] applies a change list to a snapshot (Folio-validated), so undo and redo
//! need no other planner. Pulse mints no ids: the new sequence id, mirrored track ids, nested clip
//! ids, link group and (Make Subsequence) the copies' clip and record ids come from the caller
//! ([`NestIds`], the `LinkedRazor` precedent).
//!
//! Studio readings (declared; Premiere Pro behaviour is UNVERIFIED, D5):
//! - the selection takes every named clip with its whole link group (as `timeline_edit` does);
//! - the new sequence takes the parent's `frame_rate`, mirrors ONLY the tracks that hold selected
//!   clips (same kinds, same per-kind order, names, `enabled`, `muted`, `sync_locked`, `targeted`,
//!   `volume` and `pan` kept; `locked` and `soloed` off) and holds the clips re-based so the
//!   selection starts at 0. With a video clip selected the re-base point is the selection start
//!   floored to the parent's frame grid, so video stays on whole frames;
//! - Nest puts one nested clip per kind present (order video, audio, caption, submix) on the
//!   LOWEST-index selected track of that kind (V1 / A1 side), spanning the re-base point to the
//!   selection end, source window `[0, span)` at unit speed; more than one nested clip are linked
//!   by the caller's link group. Clips keep their ids, markers and transitions (they move); a
//!   transition that no longer plays on either side is removed and listed (pulse's pruning);
//! - Make Subsequence copies the clips under fresh clip ids, their clip markers and transitions under
//!   fresh record ids (Folio keys are document-unique), keeps their link groups (scoped to the new
//!   sequence); from a range it copies the clips of the targeted tracks (every track when none is
//!   targeted) that overlap `[in, out)`, cut at the range edges (pulse's split, markers handed to the
//!   piece holding them);
//! - a locked track refuses Nest (`track_locked`, the parent changes); Make Subsequence reads it
//!   (the parent is unchanged).
//!
//! Refusals ([`NestError`], every one leaves the document unchanged): `nest_empty_selection`,
//! `nest_cycle` (the new sequence id is the parent's or one a selected clip nests, or Folio reports
//! a containment cycle; a validated acyclic document cannot otherwise gain a cycle by nesting: the
//! new sequence's only parent is the parent, its children are the parent's children), pulse codes
//! `unknown_sequence`, `unknown_clip`, `stale_revision`, `track_locked`, `duplicate_id` (a fresh id
//! repeated or already in the document, an existing sequence id, a link group in use),
//! `insufficient_ids`, `overlap` (the nested clip would collide with an unselected clip on its
//! track), `folio_rejected` (for example path-like ids, which are not Folio keys), `limit_exceeded`.
//!
//! **Resolution** (additive; `frame_layers`, `TimelineMixStream` and the bus mix keep their
//! signatures). [`NestedExtents`] answers a Sequence source with the nested sequence's duration, so
//! a nested clip trims like any clip and its trim only moves the window onto the nested sequence
//! (STU-VID-030 "opaque to timeline operations"). [`frame_layers_nested`] expands a Sequence-source
//! layer into the nested sequence's layers at the mapped source tick (speed and `source_in` applied
//! by `frame_layers` itself), recursively; the nested group's weight multiplies every inner layer's
//! weight (a group-opacity approximation, exact when the group shows one layer). [`NestedAudioSources`]
//! renders every nested sequence an audible clip uses through the existing `TimelineMixStream` (its
//! plain timeline mix: track volume, pan, mute and solo, not its bus routing; with
//! [`NestedAudioSources::with_routing`] also the clip gain and pan envelopes stored for it, lane
//! SMB-PR-NEST-2) into stereo buffers of
//! the nested sequence's length, innermost first, and answers the Sequence source with them. There
//! is no depth constant (STU-VID-030 "no fixed depth limit"): recursion is bounded by a cycle guard
//! (a sequence on the current path) and the caller's budget ([`StreamLimits::max_resident_frames`]
//! for all nested buffers together, checked before each allocation; `max_layers` for frames) and the
//! cancel token. The frame budget is a visit budget (R44 finding F-R44-7, lane SMB-R44-FOLLOWUPS-1):
//! [`frame_layers_nested`] charges EVERY layer it visits against `max_layers` (a media layer it
//! returns, a Sequence layer it expands and an unresolved one it records), before the work, so an
//! acyclic document whose sequences nest each other on k tracks (k^depth visits) is refused
//! `limit_exceeded` after at most `max_layers` visits instead of walking the fan-out (test
//! `nest::nested_frame_fan_out_is_bounded_by_the_layer_budget`). A nested sequence that cannot be rendered (an inner source missing, a refusal of the
//! inner mix, a cycle, an unknown sequence) gets a typed [`NestAudioReport`] and plays as silence of
//! its length, so the parent mix still renders and the loss is reported, never silent (V4-03; tests
//! in tests/nest.rs).
#![deny(clippy::arithmetic_side_effects)]

use crate::audio::{AudioSources, StreamLimits, TimelineMixStream};
use crate::error::PulseError;
use crate::playback::{FrameLayer, frame_layers};
use crate::range::TickRange;
use crate::time::samples_from_ticks_exact;
use crate::timeline_edit::{EditContext, SourceExtents, split_clip};
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{
    ClipSource, ClipTransitions, Code, Diagnostic, MAX_CLIPS_PER_SEQUENCE, Snapshot, Speed, StudioAudioRoutingSet, StudioClip,
    StudioClipGainEnvelope, StudioClipPanEnvelope, StudioSequence, StudioTrack, TrackKind, validate_document,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// STU-VID-041 command ids (bindings belong to the GUI).
pub const CMD_NEST: &str = "cmd.clip.nestify";
pub const CMD_MAKE_SUBSEQUENCE: &str = "cmd.sequence.makesubsequence";
pub const CMD_MAKE_SUBSEQUENCE_IN_OUT: &str = "cmd.sequence.makesubsequencefromintoout";
pub const CMD_REVEAL_NESTED: &str = "cmd.tlnav.reveal.nested.sequence";

/// Typed nest refusal: pulse's vocabulary plus the nest-specific codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NestError {
    Pulse(PulseError),
    EmptySelection,
    Cycle,
}
impl NestError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Pulse(e) => e.code(),
            Self::EmptySelection => "nest_empty_selection",
            Self::Cycle => "nest_cycle",
        }
    }
}
impl From<PulseError> for NestError {
    fn from(e: PulseError) -> Self {
        Self::Pulse(e)
    }
}
impl From<hsk_studio_accord::ValidationError> for NestError {
    fn from(e: hsk_studio_accord::ValidationError) -> Self {
        Self::Pulse(e.into())
    }
}
impl std::fmt::Display for NestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for NestError {}
pub type NestResult<T> = std::result::Result<T, NestError>;

fn folio(d: Diagnostic) -> NestError {
    match d.code {
        Code::Canceled => PulseError::Canceled.into(),
        Code::BudgetExceeded => PulseError::LimitExceeded.into(),
        Code::ContainmentCycle => NestError::Cycle,
        _ => PulseError::FolioRejected.into(),
    }
}

/// One sequence value change (the session's `SequenceEdit` shape; pulse does not depend on the
/// session): every change quotes the value it expects.
#[derive(Clone, Debug, PartialEq)]
pub enum SequenceChange {
    Replace { expect: StudioSequence, write: StudioSequence },
    Insert { at: usize, write: StudioSequence },
    Remove { expect: StudioSequence },
}

/// Caller-minted identities of one nest or subsequence (module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NestIds {
    /// The new sequence (a Folio key the document does not hold).
    pub sequence_id: String,
    pub sequence_name: String,
    /// One per mirrored track, in the parent's track order.
    pub track_ids: Vec<String>,
    /// Nest: one per nested clip (kind order video, audio, caption, submix); Make Subsequence: one
    /// per copied clip or range piece (track order, then clip order). Extra ids are unused.
    pub clip_ids: Vec<String>,
    /// Link group of the nested clips (Nest with more than one kind present).
    pub link_group: Option<String>,
    /// Make Subsequence: fresh marker and transition ids of the copies, per copy in order: its
    /// markers, then its head, then its tail transition. Extra ids are unused.
    pub record_ids: Vec<String>,
}

/// What Make Subsequence copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubsequenceSource {
    /// These clips with their link groups.
    Clips(Vec<String>),
    /// The in-to-out range on the targeted tracks (every track when none is targeted).
    Range(TickRange),
}

/// One nest or subsequence step against one base revision (module docs).
#[derive(Clone, Debug)]
pub struct NestProposal {
    command_id: &'static str,
    base_revision: u64,
    snapshot: Snapshot,
    changes: Vec<SequenceChange>,
    inverse: Vec<SequenceChange>,
    nested_clip_ids: Vec<String>,
    dropped_transitions: Vec<String>,
    copied_clips: Vec<(String, String)>,
}
impl NestProposal {
    /// STU-VID-041 command id of the step.
    pub fn command_id(&self) -> &'static str {
        self.command_id
    }
    pub fn base_revision(&self) -> u64 {
        self.base_revision
    }
    pub fn target_revision(&self) -> u64 {
        self.snapshot.document().revision
    }
    /// The prospective document, validated by Folio at `base_revision + 1`.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    /// The changes that turn the live document into [`Self::snapshot`], applied in order.
    pub fn changes(&self) -> &[SequenceChange] {
        &self.changes
    }
    /// The undo of the applied step (its own inverse is the redo: [`Self::changes`]).
    pub fn inverse(&self) -> &[SequenceChange] {
        &self.inverse
    }
    /// The id of the new sequence.
    pub fn sequence_id(&self) -> &str {
        match self.changes.first() {
            Some(SequenceChange::Insert { write, .. }) => &write.sequence_id,
            _ => "",
        }
    }
    /// Nest: the nested clips put in the parent (kind order).
    pub fn nested_clip_ids(&self) -> &[String] {
        &self.nested_clip_ids
    }
    /// Transitions removed because they no longer play (undo restores them).
    pub fn dropped_transitions(&self) -> &[String] {
        &self.dropped_transitions
    }
    /// Make Subsequence (lane SMB-PR-NEST-2): `(parent clip id, copy id)` per copied clip or range
    /// piece, in copy order, so a host can carry per-clip records (clip gain and pan envelopes) to
    /// the copies. Empty for Nest (its clips move with their ids).
    pub fn copied_clips(&self) -> &[(String, String)] {
        &self.copied_clips
    }
}

/// Applies `changes` in order to `snapshot` at exactly `expected_revision`: the result is the
/// document at revision + 1, validated by Folio (each change must quote the live value:
/// `revert_mismatch`; an inserted id that exists: `duplicate_id`; an index past the end:
/// `invalid_input`). Undo is `apply_sequence_changes(.., proposal.inverse(), ..)`.
pub fn apply_sequence_changes(snapshot: &Snapshot, expected_revision: u64, changes: &[SequenceChange], ctx: &EditContext) -> NestResult<Snapshot> {
    ctx.cancel.check().map_err(PulseError::from)?;
    let document = snapshot.document();
    if document.revision != expected_revision {
        return Err(PulseError::StaleRevision.into());
    }
    let mut sequences = document.sequences.clone();
    let position = |id: &str, list: &[StudioSequence]| list.iter().position(|s| s.sequence_id == id);
    for change in changes {
        ctx.cancel.check().map_err(PulseError::from)?;
        match change {
            SequenceChange::Replace { expect, write } => {
                let i = position(&expect.sequence_id, &sequences).ok_or(PulseError::UnknownSequence)?;
                if sequences[i] != *expect || write.sequence_id != expect.sequence_id {
                    return Err(PulseError::RevertMismatch.into());
                }
                sequences[i] = write.clone();
            }
            SequenceChange::Insert { at, write } => {
                if position(&write.sequence_id, &sequences).is_some() {
                    return Err(PulseError::DuplicateId.into());
                }
                if *at > sequences.len() {
                    return Err(PulseError::InvalidInput.into());
                }
                sequences.insert(*at, write.clone());
            }
            SequenceChange::Remove { expect } => {
                let i = position(&expect.sequence_id, &sequences).ok_or(PulseError::UnknownSequence)?;
                if sequences[i] != *expect {
                    return Err(PulseError::RevertMismatch.into());
                }
                sequences.remove(i);
            }
        }
    }
    let mut next = document.clone();
    next.sequences = sequences;
    next.revision = expected_revision.checked_add(1).ok_or(PulseError::Overflow)?;
    validate_document(next, ctx.budget, ctx.cancel, ctx.resolver).map_err(folio)
}

fn slot(kind: TrackKind) -> usize {
    match kind {
        TrackKind::Video => 0,
        TrackKind::Audio => 1,
        TrackKind::Caption => 2,
        TrackKind::Submix => 3,
    }
}
const KINDS: [TrackKind; 4] = [TrackKind::Video, TrackKind::Audio, TrackKind::Caption, TrackKind::Submix];

/// Positions `(track, clip)` of the named clips and their whole link groups, in track then clip
/// order.
fn select(seq: &StudioSequence, clip_ids: &[String], cancel: &CancellationToken) -> NestResult<Vec<(usize, usize)>> {
    if clip_ids.is_empty() {
        return Err(NestError::EmptySelection);
    }
    if clip_ids.len() > MAX_CLIPS_PER_SEQUENCE {
        return Err(PulseError::LimitExceeded.into());
    }
    let mut ids: BTreeSet<&str> = BTreeSet::new();
    let mut groups: BTreeSet<&str> = BTreeSet::new();
    for id in clip_ids {
        cancel.check().map_err(PulseError::from)?;
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| &c.clip_id == id).ok_or(PulseError::UnknownClip)?;
        ids.insert(clip.clip_id.as_str());
        if let Some(g) = clip.link_group.as_deref() {
            groups.insert(g);
        }
    }
    let mut out = Vec::new();
    for (t, track) in seq.tracks.iter().enumerate() {
        cancel.check().map_err(PulseError::from)?;
        for (c, clip) in track.clips.iter().enumerate() {
            if ids.contains(clip.clip_id.as_str()) || clip.link_group.as_deref().is_some_and(|g| groups.contains(g)) {
                out.push((t, c));
            }
        }
    }
    Ok(out)
}

/// Every sequence reachable from `from` through clip sources (including `from`).
fn reachable<'a>(sequences: &'a [StudioSequence], from: &'a str, cancel: &CancellationToken) -> NestResult<BTreeSet<&'a str>> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut stack = vec![from];
    while let Some(id) = stack.pop() {
        cancel.check().map_err(PulseError::from)?;
        if !seen.insert(id) {
            continue;
        }
        if let Some(s) = sequences.iter().find(|s| s.sequence_id == id) {
            for c in s.tracks.iter().flat_map(|t| t.clips.iter()) {
                if let ClipSource::Sequence { sequence_id } = &c.source_ref {
                    stack.push(sequence_id.as_str());
                }
            }
        }
    }
    Ok(seen)
}

/// Fresh-id checks: no repeat among `fresh`, none already a clip, track or sequence id of the
/// document, and the new sequence id is not the parent's or one a selected clip reaches (cycle).
fn check_ids(snapshot: &Snapshot, parent: &StudioSequence, positions: &[(usize, usize)], ids: &NestIds, fresh: &[&str], ctx: &EditContext) -> NestResult<()> {
    let document = snapshot.document();
    let sequences = &document.sequences;
    if ids.sequence_id == parent.sequence_id {
        return Err(NestError::Cycle);
    }
    for &(t, c) in positions {
        if let ClipSource::Sequence { sequence_id } = &parent.tracks[t].clips[c].source_ref
            && (reachable(sequences, sequence_id, ctx.cancel)?.contains(ids.sequence_id.as_str())
                || reachable(sequences, sequence_id, ctx.cancel)?.contains(parent.sequence_id.as_str()))
        {
            return Err(NestError::Cycle);
        }
    }
    if sequences.iter().any(|s| s.sequence_id == ids.sequence_id) {
        return Err(PulseError::DuplicateId.into());
    }
    let mut taken: BTreeSet<&str> = BTreeSet::new();
    for s in sequences {
        for t in &s.tracks {
            taken.insert(t.track_id.as_str());
            for c in &t.clips {
                taken.insert(c.clip_id.as_str());
                for m in &c.markers {
                    taken.insert(m.marker_id.as_str());
                }
                for tr in [&c.transitions.head, &c.transitions.tail].into_iter().flatten() {
                    taken.insert(tr.transition_id.as_str());
                }
            }
        }
        for m in &s.markers {
            taken.insert(m.marker_id.as_str());
        }
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for id in fresh {
        ctx.cancel.check().map_err(PulseError::from)?;
        if taken.contains(id) || !seen.insert(id) {
            return Err(PulseError::DuplicateId.into());
        }
    }
    Ok(())
}

fn take(list: &[String], i: usize) -> NestResult<&str> {
    list.get(i).map(String::as_str).ok_or(NestError::Pulse(PulseError::InsufficientIds))
}

/// The mirrored tracks of the parent tracks in `used` (parent order), empty.
fn mirror(parent: &StudioSequence, used: &BTreeSet<usize>, ids: &NestIds) -> NestResult<(Vec<StudioTrack>, BTreeMap<usize, usize>)> {
    let mut tracks = Vec::with_capacity(used.len());
    let mut map = BTreeMap::new();
    let mut per_kind = [0u32; 4];
    for (n, &t) in used.iter().enumerate() {
        let src = &parent.tracks[t];
        let k = slot(src.kind);
        tracks.push(StudioTrack {
            schema_id: src.schema_id.clone(),
            track_id: take(&ids.track_ids, n)?.to_owned(),
            kind: src.kind,
            index: per_kind[k],
            name: src.name.clone(),
            enabled: src.enabled,
            locked: false,
            sync_locked: src.sync_locked,
            targeted: src.targeted,
            muted: src.muted,
            soloed: false,
            clips: Vec::new(),
            volume: src.volume,
            pan: src.pan,
        });
        per_kind[k] = per_kind[k].checked_add(1).ok_or(PulseError::Overflow)?;
        map.insert(t, n);
    }
    Ok((tracks, map))
}

/// The re-base point: the earliest start, floored to the frame grid when a video clip is among
/// `clips` (module docs).
fn rebase_point(parent: &StudioSequence, clips: &[(usize, StudioClip)]) -> NestResult<(Ticks, Ticks)> {
    let start = clips.iter().map(|(_, c)| c.timeline_start).min().ok_or(NestError::EmptySelection)?;
    let mut end = Ticks::new(0);
    for (_, c) in clips {
        end = end.max(c.timeline_end().ok_or(PulseError::Overflow)?);
    }
    let video = clips.iter().any(|(t, _)| parent.tracks[*t].kind == TrackKind::Video);
    let tpf = parent.frame_rate.ticks_per_frame;
    let start = if video && tpf > 0 {
        Ticks::new(start.value().checked_div(tpf).and_then(|f| f.checked_mul(tpf)).ok_or(PulseError::Overflow)?)
    } else {
        start
    };
    Ok((start, end))
}

/// Builds the new sequence from `(parent track index, clip)` pieces re-based to `origin`.
fn build_sequence(parent: &StudioSequence, pieces: Vec<(usize, StudioClip)>, origin: Ticks, ids: &NestIds) -> NestResult<StudioSequence> {
    let used: BTreeSet<usize> = pieces.iter().map(|(t, _)| *t).collect();
    let (mut tracks, map) = mirror(parent, &used, ids)?;
    for (t, mut clip) in pieces {
        clip.timeline_start = clip.timeline_start.checked_sub(origin)?;
        let n = *map.get(&t).ok_or(PulseError::UnknownTrack)?;
        tracks[n].clips.push(clip);
    }
    for t in &mut tracks {
        t.clips.sort_by_key(|c| c.timeline_start);
    }
    let mut seq = StudioSequence {
        schema_id: parent.schema_id.clone(),
        sequence_id: ids.sequence_id.clone(),
        name: ids.sequence_name.clone(),
        frame_rate: parent.frame_rate,
        duration: Ticks::new(0),
        tracks,
        markers: Vec::new(),
        work_area: None,
    };
    seq.duration = seq.content_end().ok_or(PulseError::Overflow)?;
    Ok(seq)
}

fn prune(seq: &mut StudioSequence, sequences: &[StudioSequence], ctx: &EditContext) -> NestResult<Vec<String>> {
    let extents = NestedExtents::new(sequences, ctx.extents);
    Ok(crate::transition::prune_transitions(seq, &extents, ctx.cancel)?)
}

fn start(snapshot: &Snapshot, sequence_id: &str, expected_revision: u64, ctx: &EditContext) -> NestResult<usize> {
    ctx.cancel.check().map_err(PulseError::from)?;
    let document = snapshot.document();
    if document.revision != expected_revision {
        return Err(PulseError::StaleRevision.into());
    }
    document.sequences.iter().position(|s| s.sequence_id == sequence_id).ok_or(NestError::Pulse(PulseError::UnknownSequence))
}

#[allow(clippy::too_many_arguments)]
fn finish(
    snapshot: &Snapshot,
    expected_revision: u64,
    command_id: &'static str,
    nested: StudioSequence,
    parent: Option<(StudioSequence, StudioSequence)>,
    nested_clip_ids: Vec<String>,
    dropped_transitions: Vec<String>,
    ctx: &EditContext,
) -> NestResult<NestProposal> {
    let at = snapshot.document().sequences.len();
    let mut changes = vec![SequenceChange::Insert { at, write: nested.clone() }];
    let mut inverse = Vec::new();
    if let Some((before, after)) = parent {
        inverse.push(SequenceChange::Replace { expect: after.clone(), write: before.clone() });
        changes.push(SequenceChange::Replace { expect: before, write: after });
    }
    inverse.push(SequenceChange::Remove { expect: nested });
    let validated = apply_sequence_changes(snapshot, expected_revision, &changes, ctx)?;
    Ok(NestProposal { command_id, base_revision: expected_revision, snapshot: validated, changes, inverse, nested_clip_ids, dropped_transitions, copied_clips: Vec::new() })
}

/// Nest (STU-VID-030; module docs): the clips `clip_ids` (with their link groups) of sequence
/// `sequence_id` move into a new sequence and the parent shows one nested clip per kind instead.
pub fn plan_nest(snapshot: &Snapshot, sequence_id: &str, expected_revision: u64, clip_ids: &[String], ids: &NestIds, ctx: &EditContext) -> NestResult<NestProposal> {
    let index = start(snapshot, sequence_id, expected_revision, ctx)?;
    let document = snapshot.document();
    let parent = &document.sequences[index];
    let positions = select(parent, clip_ids, ctx.cancel)?;
    if positions.iter().any(|(t, _)| parent.tracks[*t].locked) {
        return Err(PulseError::TrackLocked.into());
    }
    let kinds: Vec<TrackKind> = KINDS.into_iter().filter(|k| positions.iter().any(|(t, _)| parent.tracks[*t].kind == *k)).collect();
    let mut fresh: Vec<&str> = Vec::new();
    let used: BTreeSet<usize> = positions.iter().map(|(t, _)| *t).collect();
    for n in 0..used.len() {
        fresh.push(take(&ids.track_ids, n)?);
    }
    for n in 0..kinds.len() {
        fresh.push(take(&ids.clip_ids, n)?);
    }
    let group = if kinds.len() > 1 { Some(ids.link_group.as_deref().ok_or(NestError::Pulse(PulseError::InsufficientIds))?) } else { None };
    check_ids(snapshot, parent, &positions, ids, &fresh, ctx)?;
    if let Some(g) = group
        && parent.tracks.iter().flat_map(|t| t.clips.iter()).any(|c| c.link_group.as_deref() == Some(g))
    {
        return Err(PulseError::DuplicateId.into());
    }
    // The selection leaves the parent and moves, re-based, into the new sequence.
    let selected: BTreeSet<(usize, usize)> = positions.iter().copied().collect();
    let pieces: Vec<(usize, StudioClip)> = positions.iter().map(|&(t, c)| (t, parent.tracks[t].clips[c].clone())).collect();
    let (origin, end) = rebase_point(parent, &pieces)?;
    let span = end.checked_sub(origin)?;
    let mut nested = build_sequence(parent, pieces, origin, ids)?;
    let mut after = parent.clone();
    for (t, track) in after.tracks.iter_mut().enumerate() {
        let mut c = 0usize;
        track.clips.retain(|_| {
            let keep = !selected.contains(&(t, c));
            c = c.saturating_add(1);
            keep
        });
    }
    let template = parent.tracks[positions[0].0].clips[positions[0].1].schema_id.clone();
    let mut nested_clip_ids = Vec::with_capacity(kinds.len());
    for (k, kind) in kinds.iter().enumerate() {
        let target = used.iter().copied().filter(|t| parent.tracks[*t].kind == *kind).min_by_key(|t| parent.tracks[*t].index).ok_or(PulseError::UnknownTrack)?;
        let clip = StudioClip {
            schema_id: template.clone(),
            clip_id: take(&ids.clip_ids, k)?.to_owned(),
            source_ref: ClipSource::Sequence { sequence_id: ids.sequence_id.clone() },
            source_in: Ticks::new(0),
            source_out: span,
            timeline_start: origin,
            duration: span,
            speed: Speed::UNIT,
            enabled: true,
            link_group: group.map(str::to_owned),
            markers: Vec::new(),
            transitions: ClipTransitions::default(),
        };
        let lane = &mut after.tracks[target].clips;
        let at = lane.partition_point(|c| c.timeline_start < origin);
        let collides_before = at.checked_sub(1).and_then(|i| lane.get(i)).is_some_and(|c| c.timeline_end().is_none_or(|e| e > origin));
        let collides_after = lane.get(at).is_some_and(|c| c.timeline_start < end);
        if collides_before || collides_after {
            return Err(PulseError::Overlap.into());
        }
        nested_clip_ids.push(clip.clip_id.clone());
        lane.insert(at, clip);
    }
    let mut prospective = document.sequences.clone();
    prospective[index] = after.clone();
    prospective.push(nested.clone());
    let mut dropped = prune(&mut nested, &prospective, ctx)?;
    if let Some(last) = prospective.last_mut() {
        last.clone_from(&nested);
    }
    dropped.extend(prune(&mut after, &prospective, ctx)?);
    after.duration = after.content_end().ok_or(PulseError::Overflow)?;
    finish(snapshot, expected_revision, CMD_NEST, nested, Some((parent.clone(), after)), nested_clip_ids, dropped, ctx)
}

/// Make Subsequence (STU-VID-030; module docs): a new sequence from the selection or the in-to-out
/// range of sequence `sequence_id`; the parent is unchanged.
pub fn plan_make_subsequence(snapshot: &Snapshot, sequence_id: &str, expected_revision: u64, source: &SubsequenceSource, ids: &NestIds, ctx: &EditContext) -> NestResult<NestProposal> {
    let index = start(snapshot, sequence_id, expected_revision, ctx)?;
    let parent = &snapshot.document().sequences[index];
    let (mut pieces, origin, command) = match source {
        SubsequenceSource::Clips(clip_ids) => {
            let positions = select(parent, clip_ids, ctx.cancel)?;
            let pieces: Vec<(usize, StudioClip)> = positions.iter().map(|&(t, c)| (t, parent.tracks[t].clips[c].clone())).collect();
            let (origin, _) = rebase_point(parent, &pieces)?;
            (pieces, origin, CMD_MAKE_SUBSEQUENCE)
        }
        SubsequenceSource::Range(span) => {
            if span.is_empty() {
                return Err(NestError::EmptySelection);
            }
            let any_targeted = parent.tracks.iter().any(|t| t.targeted);
            let mut pieces = Vec::new();
            for (t, track) in parent.tracks.iter().enumerate() {
                if any_targeted && !track.targeted {
                    continue;
                }
                for clip in &track.clips {
                    ctx.cancel.check().map_err(PulseError::from)?;
                    let whole = TickRange::from_start_duration(clip.timeline_start, clip.duration)?;
                    if !whole.overlaps(*span) {
                        continue;
                    }
                    let mut piece = clip.clone();
                    if piece.timeline_start < span.start() {
                        piece = split_clip(&piece, span.start(), &piece.clip_id)?.1;
                    }
                    if piece.timeline_end().is_some_and(|e| e > span.end()) {
                        piece = split_clip(&piece, span.end(), &piece.clip_id)?.0;
                    }
                    pieces.push((t, piece));
                }
            }
            (pieces, span.start(), CMD_MAKE_SUBSEQUENCE_IN_OUT)
        }
    };
    if pieces.is_empty() {
        return Err(NestError::EmptySelection);
    }
    // Fresh clip ids and record ids for the copies.
    let used: BTreeSet<usize> = pieces.iter().map(|(t, _)| *t).collect();
    let mut fresh: Vec<&str> = Vec::new();
    for n in 0..used.len() {
        fresh.push(take(&ids.track_ids, n)?);
    }
    let mut record = 0usize;
    let mut copied = Vec::with_capacity(pieces.len());
    for (n, (_, clip)) in pieces.iter_mut().enumerate() {
        ctx.cancel.check().map_err(PulseError::from)?;
        let id = take(&ids.clip_ids, n)?;
        fresh.push(id);
        copied.push((std::mem::replace(&mut clip.clip_id, id.to_owned()), id.to_owned()));
        for m in &mut clip.markers {
            let id = take(&ids.record_ids, record)?;
            fresh.push(id);
            m.marker_id = id.to_owned();
            record = record.checked_add(1).ok_or(PulseError::Overflow)?;
        }
        for tr in [&mut clip.transitions.head, &mut clip.transitions.tail].into_iter().flatten() {
            let id = take(&ids.record_ids, record)?;
            fresh.push(id);
            tr.transition_id = id.to_owned();
            record = record.checked_add(1).ok_or(PulseError::Overflow)?;
        }
    }
    check_ids(snapshot, parent, &[], ids, &fresh, ctx)?;
    let mut nested = build_sequence(parent, pieces, origin, ids)?;
    let mut prospective = snapshot.document().sequences.clone();
    prospective.push(nested.clone());
    let dropped = prune(&mut nested, &prospective, ctx)?;
    nested.duration = nested.content_end().ok_or(PulseError::Overflow)?;
    let mut proposal = finish(snapshot, expected_revision, command, nested, None, Vec::new(), dropped, ctx)?;
    proposal.copied_clips = copied;
    Ok(proposal)
}

// ---- resolution ----

/// [`SourceExtents`] that answers a Sequence source with the nested sequence's duration (module
/// docs) and defers every other source to `inner`.
pub struct NestedExtents<'a> {
    sequences: &'a [StudioSequence],
    inner: &'a dyn SourceExtents,
}
impl<'a> NestedExtents<'a> {
    pub fn new(sequences: &'a [StudioSequence], inner: &'a dyn SourceExtents) -> Self {
        Self { sequences, inner }
    }
}
impl SourceExtents for NestedExtents<'_> {
    fn extent(&self, source: &ClipSource) -> Option<Ticks> {
        match source {
            ClipSource::Sequence { sequence_id } => self.sequences.iter().find(|s| &s.sequence_id == sequence_id).map(|s| s.duration),
            other => self.inner.extent(other),
        }
    }
}

/// One resolved program-monitor layer: `path` names the nested clips it sits inside, outermost
/// first (empty for a layer of the sequence itself).
#[derive(Clone, Debug, PartialEq)]
pub struct NestedLayer {
    pub path: Vec<String>,
    pub layer: FrameLayer,
}

/// A nested clip that could not be expanded (`code`: `nest_cycle` or `unknown_sequence`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NestUnresolved {
    pub path: Vec<String>,
    pub clip_id: String,
    pub code: &'static str,
}

/// A frame with nested sequences expanded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NestedFrame {
    pub layers: Vec<NestedLayer>,
    pub unresolved: Vec<NestUnresolved>,
}

/// [`frame_layers`] of `sequence` at `tick` with every Sequence-source layer replaced by the
/// nested sequence's layers at that layer's source tick, recursively (module docs). `sequences` are
/// the document's sequences; more than `max_layers` visited layers in total (media, expanded
/// Sequence and unresolved layers alike, R44 F-R44-7) is `limit_exceeded`.
pub fn frame_layers_nested(sequences: &[StudioSequence], sequence: &StudioSequence, tick: Ticks, extents: &dyn SourceExtents, max_layers: usize, cancel: &CancellationToken) -> crate::Result<NestedFrame> {
    let nested = NestedExtents::new(sequences, extents);
    let mut out = NestedFrame::default();
    let mut path = Vec::new();
    let mut stack = vec![sequence.sequence_id.clone()];
    let mut visits = 0usize;
    expand(sequences, sequence, tick, 1.0, &nested, max_layers, &mut visits, &mut path, &mut stack, &mut out, cancel)?;
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn expand(
    sequences: &[StudioSequence],
    sequence: &StudioSequence,
    tick: Ticks,
    weight: f64,
    extents: &NestedExtents<'_>,
    max_layers: usize,
    visits: &mut usize,
    path: &mut Vec<String>,
    stack: &mut Vec<String>,
    out: &mut NestedFrame,
    cancel: &CancellationToken,
) -> crate::Result<()> {
    for layer in frame_layers(sequence, tick, extents, cancel)? {
        cancel.check()?;
        // Visit budget (module docs, F-R44-7): every layer is charged before its work.
        *visits = visits.checked_add(1).ok_or(PulseError::Overflow)?;
        if *visits > max_layers {
            return Err(PulseError::LimitExceeded);
        }
        let ClipSource::Sequence { sequence_id } = &layer.source else {
            let mut layer = layer;
            layer.weight *= weight;
            out.layers.push(NestedLayer { path: path.clone(), layer });
            continue;
        };
        if stack.iter().any(|s| s == sequence_id) {
            out.unresolved.push(NestUnresolved { path: path.clone(), clip_id: layer.clip_id.clone(), code: "nest_cycle" });
            continue;
        }
        let Some(inner) = sequences.iter().find(|s| &s.sequence_id == sequence_id) else {
            out.unresolved.push(NestUnresolved { path: path.clone(), clip_id: layer.clip_id.clone(), code: "unknown_sequence" });
            continue;
        };
        path.push(layer.clip_id.clone());
        stack.push(sequence_id.clone());
        let result = expand(sequences, inner, layer.source_tick, weight * layer.weight, extents, max_layers, visits, path, stack, out, cancel);
        path.pop();
        stack.pop();
        result?;
    }
    Ok(())
}

/// A nested sequence whose audio could not be rendered: it plays as silence of its length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NestAudioReport {
    pub sequence_id: String,
    /// Clips of the mixed sequences that play it.
    pub clip_ids: Vec<String>,
    pub code: &'static str,
}

/// Mixed audio of the nested sequences by id.
type Mixes = BTreeMap<String, Vec<Vec<f32>>>;

/// [`AudioSources`] that answers a Sequence source with the nested sequence's rendered mix (module
/// docs) and defers every other source to `inner`. Cheap to clone (the mixes are shared).
#[derive(Clone)]
pub struct NestedAudioSources<S> {
    inner: S,
    mixes: Arc<Mixes>,
    reports: Arc<Vec<NestAudioReport>>,
}

struct Lookup<'a, S> {
    inner: &'a S,
    mixes: &'a Mixes,
}
impl<S: AudioSources> AudioSources for Lookup<'_, S> {
    fn audio(&self, source: &ClipSource) -> Option<&[Vec<f32>]> {
        match source {
            ClipSource::Sequence { sequence_id } => self.mixes.get(sequence_id).map(Vec::as_slice),
            other => self.inner.audio(other),
        }
    }
}
impl<S: AudioSources> AudioSources for NestedAudioSources<S> {
    fn audio(&self, source: &ClipSource) -> Option<&[Vec<f32>]> {
        match source {
            ClipSource::Sequence { sequence_id } => self.mixes.get(sequence_id).map(Vec::as_slice),
            other => self.inner.audio(other),
        }
    }
}

/// Enabled clips on audio tracks of `seq` that nest a sequence: `(clip id, nested id)`.
fn audio_nests(seq: &StudioSequence) -> Vec<(String, String)> {
    seq.tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .flat_map(|t| t.clips.iter())
        .filter(|c| c.enabled)
        .filter_map(|c| match &c.source_ref {
            ClipSource::Sequence { sequence_id } => Some((c.clip_id.clone(), sequence_id.clone())),
            _ => None,
        })
        .collect()
}

impl<S: AudioSources> NestedAudioSources<S> {
    /// Renders every nested sequence the audible clips of `root` use (recursively, innermost
    /// first) at `sample_rate_hz` (module docs). Refusals: `canceled`, `limit_exceeded` when the
    /// nested buffers together would pass `limits.max_resident_frames` (before allocating). The
    /// nested mixes carry no clip envelopes: [`NestedAudioSources::with_routing`] with an empty set.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sequences: &[StudioSequence],
        root: &StudioSequence,
        inner: S,
        sample_rate_hz: u32,
        max_block_frames: usize,
        extents: &dyn SourceExtents,
        limits: StreamLimits,
        cancel: &CancellationToken,
    ) -> crate::Result<Self> {
        Self::with_routing(sequences, root, inner, &StudioAudioRoutingSet::default(), sample_rate_hz, max_block_frames, extents, limits, cancel)
    }

    /// As [`NestedAudioSources::new`], each nested sequence's mix applying the clip gain and pan
    /// envelopes of its own entry in `routing` (lane SMB-PR-NEST-2, `TimelineMixStream::
    /// with_clip_envelopes`). Envelope offsets are ticks from the clip's own `timeline_start`, so a
    /// clip nested with its envelope plays as it did in the parent (test
    /// `nested_mix_applies_moved_clip_envelopes` in tests/nest.rs). Only the clip envelopes are
    /// read: a nested sequence's bus routes, sends, effects and track automation do not apply (the
    /// nested mix stays its timeline mix). An envelope the mix refuses (an offset off the sample
    /// grid, a gain above +12 dB) makes that nested sequence silent and reported, like any other
    /// unrenderable nested sequence.
    #[allow(clippy::too_many_arguments)]
    pub fn with_routing(
        sequences: &[StudioSequence],
        root: &StudioSequence,
        inner: S,
        routing: &StudioAudioRoutingSet,
        sample_rate_hz: u32,
        max_block_frames: usize,
        extents: &dyn SourceExtents,
        limits: StreamLimits,
        cancel: &CancellationToken,
    ) -> crate::Result<Self> {
        cancel.check()?;
        let nested_extents = NestedExtents::new(sequences, extents);
        let mut mixes: Mixes = BTreeMap::new();
        let mut reports: Vec<NestAudioReport> = Vec::new();
        let mut users: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut resident = 0u64;
        // Depth-first post-order with an explicit stack: (sequence id, children pushed).
        let mut todo: Vec<(String, bool)> = audio_nests(root).into_iter().map(|(clip, id)| {
            users.entry(id.clone()).or_default().push(clip);
            (id, false)
        }).collect();
        let mut on_path: Vec<String> = vec![root.sequence_id.clone()];
        while let Some((id, expanded)) = todo.pop() {
            cancel.check()?;
            if expanded {
                on_path.pop();
                if mixes.contains_key(&id) {
                    continue;
                }
                let Some(seq) = sequences.iter().find(|s| s.sequence_id == id) else { continue };
                let frames = frames_of(seq.duration, sample_rate_hz);
                resident = resident.checked_add(frames).ok_or(PulseError::Overflow)?;
                if resident > limits.max_resident_frames {
                    return Err(PulseError::LimitExceeded);
                }
                let len = usize::try_from(frames).map_err(|_| PulseError::Overflow)?;
                let own = routing.routings.iter().find(|r| r.sequence_id == id);
                let envelopes = (own.map_or(&[][..], |r| r.clip_gains.as_slice()), own.map_or(&[][..], |r| r.clip_pans.as_slice()));
                let rendered = {
                    match render(seq, sample_rate_hz, max_block_frames, &Lookup { inner: &inner, mixes: &mixes }, &nested_extents, limits, envelopes, len, cancel) {
                        Ok(buffers) => Some(buffers),
                        Err(PulseError::Canceled) => return Err(PulseError::Canceled),
                        Err(e) => {
                            reports.push(NestAudioReport { sequence_id: id.clone(), clip_ids: users.get(&id).cloned().unwrap_or_default(), code: e.code() });
                            None
                        }
                    }
                };
                mixes.insert(id, rendered.unwrap_or_else(|| vec![vec![0.0; len], vec![0.0; len]]));
                continue;
            }
            if mixes.contains_key(&id) {
                continue;
            }
            if on_path.contains(&id) {
                reports.push(NestAudioReport { sequence_id: id.clone(), clip_ids: users.get(&id).cloned().unwrap_or_default(), code: "nest_cycle" });
                continue;
            }
            let Some(seq) = sequences.iter().find(|s| s.sequence_id == id) else {
                reports.push(NestAudioReport { sequence_id: id.clone(), clip_ids: users.get(&id).cloned().unwrap_or_default(), code: "unknown_sequence" });
                continue;
            };
            on_path.push(id.clone());
            todo.push((id.clone(), true));
            for (clip, child) in audio_nests(seq) {
                users.entry(child.clone()).or_default().push(clip);
                if on_path.contains(&child) {
                    reports.push(NestAudioReport { sequence_id: child.clone(), clip_ids: users.get(&child).cloned().unwrap_or_default(), code: "nest_cycle" });
                    continue;
                }
                todo.push((child, false));
            }
        }
        Ok(Self { inner, mixes: Arc::new(mixes), reports: Arc::new(reports) })
    }

    /// The nested sequences that play as silence and why.
    pub fn reports(&self) -> &[NestAudioReport] {
        &self.reports
    }
    /// The rendered mix of nested sequence `sequence_id` (planar stereo).
    pub fn mix(&self, sequence_id: &str) -> Option<&[Vec<f32>]> {
        self.mixes.get(sequence_id).map(Vec::as_slice)
    }
    pub fn inner(&self) -> &S {
        &self.inner
    }
}

/// Whole samples of `duration` at `rate` (floored).
fn frames_of(duration: Ticks, rate: u32) -> u64 {
    let product = u128::from(duration.value()).saturating_mul(u128::from(rate));
    let per_second = u128::from(hsk_studio_accord::TICKS_PER_SECOND);
    u64::try_from(product.checked_div(per_second).unwrap_or(0)).unwrap_or(u64::MAX)
}

#[allow(clippy::too_many_arguments)]
fn render<S: AudioSources>(
    seq: &StudioSequence,
    rate: u32,
    block: usize,
    sources: &Lookup<'_, S>,
    extents: &dyn SourceExtents,
    limits: StreamLimits,
    (gains, pans): (&[StudioClipGainEnvelope], &[StudioClipPanEnvelope]),
    len: usize,
    cancel: &CancellationToken,
) -> crate::Result<Vec<Vec<f32>>> {
    // The window onto the nested sequence must sit on the sample grid (positions inside do too).
    samples_from_ticks_exact(seq.duration, rate)?;
    let mut stream = TimelineMixStream::with_clip_envelopes(seq, rate, block, Lookup { inner: sources.inner, mixes: sources.mixes }, extents, limits, gains, pans, cancel)?;
    let (mut left, mut right) = (vec![0.0f32; len], vec![0.0f32; len]);
    let mut at = 0usize;
    let block = block.max(1);
    while at < len {
        let n = block.min(len.saturating_sub(at));
        let end = at.saturating_add(n);
        stream.render(&mut left[at..end], &mut right[at..end], cancel)?;
        at = end;
    }
    Ok(vec![left, right])
}
