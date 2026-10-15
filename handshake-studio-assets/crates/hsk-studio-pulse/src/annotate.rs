//! Timeline annotations beside one Folio sequence: markers (STU-VID-034) as typed records with
//! their own revision and one-step inverse. Folio stores no markers yet (folio `timeline.rs`
//! module docs), so pulse owns the record shape and its validation and the host persists the set;
//! clip markers follow clip edits through [`follow_timeline_edit`].
//!
//! Scopes: a SEQUENCE marker sits in sequence ticks and never moves with clips; a CLIP marker sits
//! in the clip's SOURCE ticks, so it travels with the clip when the clip moves or ripples, and a
//! trim or slip only changes whether it is inside the playing window. A clip cut in two (razor,
//! insert, overwrite, range lift/extract) hands each marker to the piece whose source window holds
//! the marker start. Project-item markers live on media items and are not timeline records.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::{PulseError, Result};
use crate::range::{Grid, TickDelta, TickRange};
use crate::timeline_edit::{EditProposal, rate_of};
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{StudioClip, StudioSequence};
use std::collections::{BTreeMap, BTreeSet};

/// Most markers one annotation set holds.
pub const MAX_MARKERS: usize = 65_536;
/// Longest id, name, comment or payload text in bytes.
pub const MAX_ANNOTATION_TEXT_BYTES: usize = 4096;
const MAX_ID_BYTES: usize = 128;
const CANCEL_STRIDE: usize = 256;

/// The 16 declared label colours (STU-VID-020 `label_color`), reused for markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LabelColor {
    Violet,
    Iris,
    Caribbean,
    Lavender,
    Cerulean,
    Forest,
    Rose,
    Mango,
    Purple,
    Blue,
    Teal,
    Magenta,
    Tan,
    Green,
    Brown,
    Yellow,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkerScope {
    /// Sequence time; does not move with clips.
    Sequence,
    /// The clip's SOURCE time; travels with the clip.
    Clip { clip_id: String },
}

/// The optional typed role of a marker (STU-VID-034).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkerRole {
    Chapter,
    WebLink { url: String },
    CuePoint,
    Caption { text: String },
}

/// A point (`duration == 0`) or span marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    pub marker_id: String,
    pub scope: MarkerScope,
    pub name: String,
    pub comment: String,
    pub color: LabelColor,
    pub start: Ticks,
    pub duration: Ticks,
    pub role: Option<MarkerRole>,
}

/// Markers of one sequence with their own revision (the Folio document revision is separate).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineAnnotations {
    sequence_id: String,
    revision: u64,
    markers: Vec<Marker>,
}

impl TimelineAnnotations {
    /// An empty set at revision 0.
    pub fn new(sequence_id: &str) -> Self {
        Self {
            sequence_id: sequence_id.to_owned(),
            revision: 0,
            markers: Vec::new(),
        }
    }
    /// A stored set, validated against `sequence` (the host's persisted form).
    pub fn from_parts(
        sequence: &StudioSequence,
        revision: u64,
        markers: Vec<Marker>,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        if markers.len() > MAX_MARKERS {
            return Err(PulseError::LimitExceeded);
        }
        let set = Self {
            sequence_id: sequence.sequence_id.clone(),
            revision,
            markers,
        };
        let mut ids = BTreeSet::new();
        for (i, marker) in set.markers.iter().enumerate() {
            if i.is_multiple_of(CANCEL_STRIDE) {
                cancel.check()?;
            }
            validate_marker(sequence, marker)?;
            if !ids.insert(marker.marker_id.as_str()) {
                return Err(PulseError::DuplicateId);
            }
        }
        Ok(set)
    }
    pub fn sequence_id(&self) -> &str {
        &self.sequence_id
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn markers(&self) -> &[Marker] {
        &self.markers
    }
    pub fn marker(&self, marker_id: &str) -> Option<&Marker> {
        self.markers.iter().find(|m| m.marker_id == marker_id)
    }
}

/// Typed annotation commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnotationEdit {
    AddMarker(Marker),
    /// Replaces the marker with the same id (name, comment, colour, position, span, role).
    ReplaceMarker(Marker),
    RemoveMarker {
        marker_id: String,
    },
}

/// The reversible half of one annotation step (`expect` = after, `restore` = before).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationInverse {
    expect: TimelineAnnotations,
    restore: TimelineAnnotations,
}

/// One annotation step at `revision + 1` with its inverse and what a followed edit dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationProposal {
    annotations: TimelineAnnotations,
    inverse: AnnotationInverse,
    dropped_markers: Vec<String>,
}

impl AnnotationProposal {
    pub fn annotations(&self) -> &TimelineAnnotations {
        &self.annotations
    }
    pub fn inverse(&self) -> &AnnotationInverse {
        &self.inverse
    }
    /// Clip markers whose clip the followed edit removed (reported, never silently lost).
    pub fn dropped_markers(&self) -> &[String] {
        &self.dropped_markers
    }
}

fn text_ok(s: &str, max: usize) -> bool {
    s.len() <= max && !s.chars().any(char::is_control)
}

fn find_clip<'a>(sequence: &'a StudioSequence, clip_id: &str) -> Option<&'a StudioClip> {
    sequence
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .find(|c| c.clip_id == clip_id)
}

fn validate_marker(sequence: &StudioSequence, marker: &Marker) -> Result<()> {
    let role_ok = match &marker.role {
        Some(MarkerRole::WebLink { url }) => {
            !url.is_empty() && text_ok(url, MAX_ANNOTATION_TEXT_BYTES)
        }
        Some(MarkerRole::Caption { text }) => text_ok(text, MAX_ANNOTATION_TEXT_BYTES),
        _ => true,
    };
    if marker.marker_id.is_empty()
        || !text_ok(&marker.marker_id, MAX_ID_BYTES)
        || !text_ok(&marker.name, MAX_ANNOTATION_TEXT_BYTES)
        || marker.comment.len() > MAX_ANNOTATION_TEXT_BYTES
        || !role_ok
    {
        return Err(PulseError::InvalidInput);
    }
    marker.start.checked_add(marker.duration)?;
    match &marker.scope {
        MarkerScope::Sequence => {
            let grid = Grid::Frames(rate_of(sequence)?);
            grid.check_position(marker.start)?;
            grid.check_delta(TickDelta::between(Ticks::new(0), marker.duration))
        }
        MarkerScope::Clip { clip_id } => find_clip(sequence, clip_id)
            .map(|_| ())
            .ok_or(PulseError::UnknownClip),
    }
}

fn require(
    annotations: &TimelineAnnotations,
    sequence: &StudioSequence,
    expected: u64,
) -> Result<u64> {
    if annotations.sequence_id != sequence.sequence_id {
        return Err(PulseError::UnknownSequence);
    }
    if annotations.revision != expected {
        return Err(PulseError::StaleRevision);
    }
    expected.checked_add(1).ok_or(PulseError::Overflow)
}

fn step(
    before: &TimelineAnnotations,
    mut after: TimelineAnnotations,
    target: u64,
    dropped: Vec<String>,
) -> AnnotationProposal {
    after.revision = target;
    AnnotationProposal {
        inverse: AnnotationInverse {
            expect: after.clone(),
            restore: before.clone(),
        },
        annotations: after,
        dropped_markers: dropped,
    }
}

/// Plans one annotation command against `annotations` at exactly `expected_revision`.
pub fn plan_annotation_edit(
    annotations: &TimelineAnnotations,
    sequence: &StudioSequence,
    expected_revision: u64,
    edit: &AnnotationEdit,
    cancel: &CancellationToken,
) -> Result<AnnotationProposal> {
    cancel.check()?;
    let target = require(annotations, sequence, expected_revision)?;
    let mut after = annotations.clone();
    match edit {
        AnnotationEdit::AddMarker(marker) => {
            if annotations.markers.len() >= MAX_MARKERS {
                return Err(PulseError::LimitExceeded);
            }
            validate_marker(sequence, marker)?;
            if annotations.marker(&marker.marker_id).is_some() {
                return Err(PulseError::DuplicateId);
            }
            after.markers.push(marker.clone());
        }
        AnnotationEdit::ReplaceMarker(marker) => {
            validate_marker(sequence, marker)?;
            let slot = after
                .markers
                .iter_mut()
                .find(|m| m.marker_id == marker.marker_id)
                .ok_or(PulseError::UnknownMarker)?;
            if slot == marker {
                return Err(PulseError::NoChange);
            }
            *slot = marker.clone();
        }
        AnnotationEdit::RemoveMarker { marker_id } => {
            let index = after
                .markers
                .iter()
                .position(|m| &m.marker_id == marker_id)
                .ok_or(PulseError::UnknownMarker)?;
            after.markers.remove(index);
        }
    }
    Ok(step(annotations, after, target, Vec::new()))
}

/// Undo or redo of one annotation step: restores `inverse.restore` only while the set equals
/// `inverse.expect` (else `revert_mismatch`); the result's inverse is the redo.
pub fn plan_annotation_revert(
    annotations: &TimelineAnnotations,
    sequence: &StudioSequence,
    expected_revision: u64,
    inverse: &AnnotationInverse,
    cancel: &CancellationToken,
) -> Result<AnnotationProposal> {
    cancel.check()?;
    let target = require(annotations, sequence, expected_revision)?;
    let mut current = annotations.clone();
    current.revision = inverse.expect.revision;
    if current != inverse.expect {
        return Err(PulseError::RevertMismatch);
    }
    Ok(step(
        annotations,
        inverse.restore.clone(),
        target,
        Vec::new(),
    ))
}

/// Carries the annotations over one applied timeline edit (the host records both steps as one
/// history entry): clip markers of a split clip move to the piece whose source window holds their
/// start; clip markers of a removed clip are dropped and listed. Sequence markers never move.
pub fn follow_timeline_edit(
    annotations: &TimelineAnnotations,
    proposal: &EditProposal,
    expected_revision: u64,
    cancel: &CancellationToken,
) -> Result<AnnotationProposal> {
    cancel.check()?;
    let sequence = proposal
        .snapshot()
        .document()
        .sequences
        .iter()
        .find(|s| s.sequence_id == proposal.sequence_id())
        .ok_or(PulseError::UnknownSequence)?;
    let target = require(annotations, sequence, expected_revision)?;
    // Every piece an origin clip was cut into (the origin keeps its id), in split order.
    let mut pieces: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (origin, new) in proposal.splits() {
        pieces
            .entry(origin.as_str())
            .or_default()
            .push(new.as_str());
    }
    let mut after = annotations.clone();
    let mut dropped = Vec::new();
    let mut kept = Vec::with_capacity(after.markers.len());
    for (i, mut marker) in std::mem::take(&mut after.markers).into_iter().enumerate() {
        if i.is_multiple_of(CANCEL_STRIDE) {
            cancel.check()?;
        }
        if let MarkerScope::Clip { clip_id } = &marker.scope {
            let candidates = std::iter::once(clip_id.as_str())
                .chain(pieces.get(clip_id.as_str()).into_iter().flatten().copied());
            let holder = candidates
                .filter_map(|id| find_clip(sequence, id))
                .find(|c| c.source_in <= marker.start && marker.start < c.source_out)
                .map(|c| c.clip_id.clone());
            match holder {
                Some(id) => marker.scope = MarkerScope::Clip { clip_id: id },
                None if find_clip(sequence, clip_id).is_some() => {}
                None => {
                    dropped.push(marker.marker_id);
                    continue;
                }
            }
        }
        kept.push(marker);
    }
    after.markers = kept;
    Ok(step(annotations, after, target, dropped))
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

/// Where `marker` sits in sequence time (`None`: a clip marker outside the clip's playing window).
pub fn marker_span(sequence: &StudioSequence, marker: &Marker) -> Result<Option<TickRange>> {
    match &marker.scope {
        MarkerScope::Sequence => {
            TickRange::from_start_duration(marker.start, marker.duration).map(Some)
        }
        MarkerScope::Clip { clip_id } => {
            let clip = find_clip(sequence, clip_id).ok_or(PulseError::UnknownClip)?;
            source_to_timeline(
                clip,
                marker.start,
                marker.start.checked_add(marker.duration)?,
            )
        }
    }
}

/// Every visible marker as `(marker_id, sequence span)`, ordered by start then id.
pub fn marker_spans(
    sequence: &StudioSequence,
    annotations: &TimelineAnnotations,
    cancel: &CancellationToken,
) -> Result<Vec<(String, TickRange)>> {
    let mut out = Vec::with_capacity(annotations.markers.len());
    for (i, marker) in annotations.markers.iter().enumerate() {
        if i.is_multiple_of(CANCEL_STRIDE) {
            cancel.check()?;
        }
        if let Some(span) = marker_span(sequence, marker)? {
            out.push((marker.marker_id.clone(), span));
        }
    }
    out.sort_by(|a, b| (a.1.start(), &a.0).cmp(&(b.1.start(), &b.0)));
    Ok(out)
}

/// Playhead navigation (STU-VID-035): the first marker start strictly after `after`.
pub fn next_marker(
    sequence: &StudioSequence,
    annotations: &TimelineAnnotations,
    after: Ticks,
    cancel: &CancellationToken,
) -> Result<Option<Ticks>> {
    Ok(marker_spans(sequence, annotations, cancel)?
        .into_iter()
        .map(|(_, span)| span.start())
        .find(|start| *start > after))
}

/// Playhead navigation (STU-VID-035): the last marker start strictly before `before`.
pub fn prev_marker(
    sequence: &StudioSequence,
    annotations: &TimelineAnnotations,
    before: Ticks,
    cancel: &CancellationToken,
) -> Result<Option<Ticks>> {
    Ok(marker_spans(sequence, annotations, cancel)?
        .into_iter()
        .map(|(_, span)| span.start())
        .rfind(|start| *start < before))
}
