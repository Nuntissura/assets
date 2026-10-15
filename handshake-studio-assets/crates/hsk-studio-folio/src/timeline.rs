//! Timeline members of the one `StudioDocument` graph: `StudioSequence`, `StudioTrack`, `StudioClip`
//! (spec STU-VID-010, STU-VID-020, STU-VID-021). Every time value is `hsk_studio_accord::Ticks`
//! (254016000000 per second, STU-VID-012) and is a plain unsigned integer on the wire. Frame-rate
//! conversion, timecode and edit arithmetic belong to Pulse; this module stores and validates only.
//!
//! Declared v02.215 gaps, not invented here: no identity prefix is specified for a sequence, so
//! `sequence_id` is a document-local key (`[A-Za-z0-9_-]{1,128}`); the spec lists both a `reversed`
//! flag and "reverse is a negative speed", so reversal is carried by the speed sign only; the sequence
//! `duration` is not defined by the spec and is stored redundantly as the end of the last clip, like
//! clip `duration`. Settings record, work area, effect stack, label colour and track height/routing
//! are not stored yet.
//!
//! Markers and transitions (lane SMB-FOLIO-TIMELINE-RECORDS): a sequence owns "a marker list"
//! (STU-VID-010) and a clip carries `markers` ("Clip-local marker list") and `transitions`
//! ("Optional head and tail transition references", STU-VID-020). Each marker carries "a name, a
//! comment, a colour, a duration (a marker may be a span, not only a point), and an optional typed
//! payload for chapter, web-link, cue-point and caption roles" (STU-VID-034); a `StudioTransition`
//! (`hsk.studio.transition@1`, Rust type `TimelineTransition`) carries `transition_kind`, `duration`, `alignment` and
//! `center_offset` (STU-VID-032). Studio readings, not stated by the spec: sequence markers sit in
//! sequence ticks and clip markers in the clip's SOURCE ticks (they travel with the clip); a
//! transition at a cut is stored once, as the outgoing clip's `tail`, and the abutting incoming
//! clip's `head` must then be empty; marker and transition ids are document-local keys (no prefix
//! is declared). Lane SMB-PULSE-PLAYBACK adds the sequence `work_area` (STU-VID-035 "a named
//! in/out span used to scope rendering and export"), the transition parameter values (one per
//! `param_key` of STU-FX-103; Studio reading: the stored instance holds values, the descriptor
//! holds bounds) and the declared shortfall choice `repeat_end_frame` (STU-VID-032 "repeat the
//! end frame") and the STU-FX-137 track `volume` (linear, default 1.0) and `pan` (default 0).
//! Every record field is omitted from the wire when empty or false, so documents
//! without them encode byte-identically. Exact
//! placement around the cut, handles and frame alignment are Pulse's; this module stores and
//! validates only.
#![deny(clippy::arithmetic_side_effects)]

use super::{
    Budget, Code, ContentDigest, Diagnostic, Resolution, ResolutionIssue, Resolver, Result,
    StudioDocument, canceled, cycle_check, err, id, key, required,
};
use hsk_studio_accord::{FrameRate as AccordFrameRate, SchemaId, Ticks};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeSet;

schema_token!(SequenceSchema, "hsk.studio.sequence@1");
schema_token!(TrackSchema, "hsk.studio.track@1");
schema_token!(ClipSchema, "hsk.studio.clip@1");
schema_token!(TransitionSchema, "hsk.studio.transition@1");

/// Hard ceilings independent of the caller `Budget` (which also counts every sequence, track and clip
/// against `Budget::nodes`).
pub const MAX_SEQUENCES: usize = 64;
pub const MAX_TRACKS_PER_SEQUENCE: usize = 1024;
pub const MAX_CLIPS_PER_SEQUENCE: usize = 262_144;
/// Longest opaque handle (manifest id, digest part, object id).
pub const MAX_HANDLE_BYTES: usize = 256;
/// Most markers (sequence and clip markers together) in one sequence.
pub const MAX_MARKERS_PER_SEQUENCE: usize = 65_536;
/// Longest marker name, comment or role text in bytes.
pub const MAX_MARKER_TEXT_BYTES: usize = 4096;
/// Most parameter values one transition carries.
pub const MAX_TRANSITION_PARAMETERS: usize = 256;

mod ticks_wire {
    use super::{Deserialize, Deserializer, Serializer, Ticks};
    pub(super) fn serialize<S: Serializer>(
        t: &Ticks,
        s: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_u64(t.value())
    }
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<Ticks, D::Error> {
        u64::deserialize(d).map(Ticks::new)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
    Caption,
    Submix,
}
impl TrackKind {
    fn slot(self) -> usize {
        match self {
            Self::Video => 0,
            Self::Audio => 1,
            Self::Caption => 2,
            Self::Submix => 3,
        }
    }
}

/// STU-VID-011 `frame_rate`: ticks per frame, never a float. Must be a canonical STU-VID-013 table
/// value; legacy exact-decimal spellings are normalised by the importer, not accepted here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SequenceFrameRate {
    pub ticks_per_frame: u64,
}
impl SequenceFrameRate {
    /// Accord's canonical frame rate when the value is a canonical table entry. The frames-per-second
    /// fraction is Accord's `FrameRate::fps_fraction`; other frame-rate conversions are Pulse's.
    pub fn accord(&self) -> Option<AccordFrameRate> {
        match AccordFrameRate::from_import(self.ticks_per_frame) {
            Ok((rate, normalization)) if !normalization.changed => Some(rate),
            _ => None,
        }
    }
}

/// The 16 declared label colours (STU-VID-020 `label_color`), used by markers.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
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

/// STU-VID-034 typed marker payload roles.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub enum MarkerRole {
    Chapter,
    WebLink { url: String },
    CuePoint,
    Caption { text: String },
}

/// A point (`duration == 0`) or span marker (STU-VID-034).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMarker {
    /// Document-local key, unique among the document's markers.
    pub marker_id: String,
    pub name: String,
    pub comment: String,
    pub color: LabelColor,
    /// Sequence ticks for a sequence marker, SOURCE ticks for a clip marker.
    #[serde(with = "ticks_wire")]
    #[schemars(with = "u64")]
    pub start: Ticks,
    #[serde(with = "ticks_wire")]
    #[schemars(with = "u64")]
    pub duration: Ticks,
    /// Explicit null when the marker has no typed role.
    #[serde(deserialize_with = "required")]
    #[schemars(required)]
    pub role: Option<MarkerRole>,
}

/// STU-VID-032 `alignment` (`custom` places the centre at `cut + center_offset`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransitionAlignment {
    CenteredOnCut,
    StartAtCut,
    EndAtCut,
    Custom,
}

/// STU-VID-032 `StudioTransition` (`hsk.studio.transition@1`). The Rust name differs because
/// folio's `StudioTransition` is the STU-PRO-116 prototype transition (reactions.rs); the wire
/// schema id is the spec's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimelineTransition {
    pub schema_id: TransitionSchema,
    /// Document-local key, unique among the document's transitions.
    pub transition_id: String,
    /// A `filter_kind` of the `transition` category (STU-FX-126), stored as an opaque key.
    pub transition_kind: String,
    #[serde(with = "ticks_wire")]
    #[schemars(with = "u64")]
    pub duration: Ticks,
    pub alignment: TransitionAlignment,
    /// Signed ticks from the cut to the transition centre; non-zero only with `custom`.
    pub center_offset: i64,
    /// Parameter values by `param_key` (STU-FX-103), unique keys (omitted when empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<TransitionParameter>,
    /// The declared shortfall choice "repeat the end frame" (omitted when false).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repeat_end_frame: bool,
}

/// One transition parameter value (STU-FX-103 `param_key` + value).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransitionParameter {
    pub param_key: String,
    /// Named `param_value` on the wire: an object key `value` is Folio's length shape.
    pub param_value: TransitionParameterValue,
}

/// A parameter value (`{"number": 0.5}`, `{"boolean": true}`, `{"enumeration": "linear"}`,
/// `{"ticks": 3}`); numbers are finite (validated), so equality is reflexive (`Eq`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TransitionParameterValue {
    Number(f64),
    Boolean(bool),
    /// An enumeration option key.
    Enumeration(String),
    Ticks(u64),
}
impl Eq for TransitionParameterValue {}

/// STU-VID-035 work area: `[start, end)` in sequence ticks, `start < end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkArea {
    #[serde(with = "ticks_wire")]
    #[schemars(with = "u64")]
    pub start: Ticks,
    #[serde(with = "ticks_wire")]
    #[schemars(with = "u64")]
    pub end: Ticks,
}

/// A clip's head and tail transitions (STU-VID-020 `transitions`; explicit nulls on the wire).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClipTransitions {
    #[serde(deserialize_with = "required")]
    #[schemars(required)]
    pub head: Option<TimelineTransition>,
    #[serde(deserialize_with = "required")]
    #[schemars(required)]
    pub tail: Option<TimelineTransition>,
}
impl ClipTransitions {
    pub fn is_empty(&self) -> bool {
        self.head.is_none() && self.tail.is_none()
    }
}

/// STU-VID-031 rational speed multiplier; the sign is the direction (negative = reverse). Must be
/// reduced and nonzero so one speed has one spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Speed {
    pub numerator: i64,
    pub denominator: u64,
}
impl Speed {
    pub const UNIT: Speed = Speed {
        numerator: 1,
        denominator: 1,
    };
    pub fn is_reversed(&self) -> bool {
        self.numerator < 0
    }
    fn valid(&self) -> bool {
        self.numerator != 0
            && self.denominator != 0
            && gcd(self.numerator.unsigned_abs(), self.denominator) == 1
    }
}

/// STU-VID-020 `source_ref`: content-addressed (never a filesystem path).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "source_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClipSource {
    /// Media item addressed like `TileRef`: caller-owned manifest handle plus content digest.
    Media {
        artifact_manifest_id: String,
        content_digest: ContentDigest,
    },
    /// Nested sequence in the same document; nesting must be acyclic.
    Sequence { sequence_id: String },
    /// Another typed primitive (for example a composition), resolved by its owner.
    Primitive {
        schema_id: String,
        object_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioClip {
    pub schema_id: ClipSchema,
    /// `SCLP-{uuid_v7}`, unique in the document.
    pub clip_id: String,
    pub source_ref: ClipSource,
    /// Window in the SOURCE time base; `source_in < source_out`.
    #[serde(with = "ticks_wire")]
    #[schemars(with = "u64")]
    pub source_in: Ticks,
    #[serde(with = "ticks_wire")]
    #[schemars(with = "u64")]
    pub source_out: Ticks,
    /// First tick of the clip on its track.
    #[serde(with = "ticks_wire")]
    #[schemars(with = "u64")]
    pub timeline_start: Ticks,
    /// Derived `(source_out - source_in) * denominator / |numerator|`, stored and validated exactly.
    #[serde(with = "ticks_wire")]
    #[schemars(with = "u64")]
    pub duration: Ticks,
    pub speed: Speed,
    pub enabled: bool,
    /// Optional link-group id; explicit null when absent.
    #[serde(deserialize_with = "required")]
    #[schemars(required)]
    pub link_group: Option<String>,
    /// Clip-local markers in SOURCE ticks (omitted from the wire when empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<StudioMarker>,
    /// Head and tail transitions (omitted from the wire when both are empty).
    #[serde(default, skip_serializing_if = "ClipTransitions::is_empty")]
    pub transitions: ClipTransitions,
}
impl StudioClip {
    /// Exclusive end tick on the timeline; `None` on tick overflow.
    pub fn timeline_end(&self) -> Option<Ticks> {
        self.timeline_start.checked_add(self.duration).ok()
    }
    /// Duration implied by the source window and speed; `None` when inexact or invalid.
    pub fn derived_duration(&self) -> Option<Ticks> {
        let span = self
            .source_out
            .value()
            .checked_sub(self.source_in.value())?;
        derived_duration(span, &self.speed).map(Ticks::new)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTrack {
    pub schema_id: TrackSchema,
    /// `STRK-{uuid_v7}`, unique in the document.
    pub track_id: String,
    pub kind: TrackKind,
    /// Position within its kind's stack; must equal the track's order among tracks of that kind.
    pub index: u32,
    pub name: String,
    pub enabled: bool,
    pub locked: bool,
    pub sync_locked: bool,
    pub targeted: bool,
    pub muted: bool,
    pub soloed: bool,
    /// Ordered by `timeline_start`; clips never overlap (touching is allowed).
    pub clips: Vec<StudioClip>,
    /// STU-FX-137 track `volume`: linear gain, default 1.0, `0.0..=MAX_TRACK_VOLUME` (omitted at
    /// the default; lane SMB-PULSE-PLAYBACK).
    #[serde(default = "unit_gain", skip_serializing_if = "is_unit_gain")]
    pub volume: f64,
    /// STU-FX-137 track `pan`: `-1.0` (left) ..= `1.0` (right), default 0 (omitted at the default).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pan: f64,
}
/// Largest track volume (linear gain; about +24 dB). Studio bound: the spec gives the default only.
pub const MAX_TRACK_VOLUME: f64 = 16.0;
fn unit_gain() -> f64 {
    1.0
}
#[allow(clippy::float_cmp)]
fn is_unit_gain(v: &f64) -> bool {
    *v == 1.0
}
#[allow(clippy::float_cmp)]
fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSequence {
    pub schema_id: SequenceSchema,
    /// Document-local key (no sequence identity prefix is declared in the spec).
    pub sequence_id: String,
    pub name: String,
    pub frame_rate: SequenceFrameRate,
    /// End of the last clip over all tracks (0 when empty), stored redundantly and validated.
    #[serde(with = "ticks_wire")]
    #[schemars(with = "u64")]
    pub duration: Ticks,
    /// Ordered stack; per-kind `index` values follow this order.
    pub tracks: Vec<StudioTrack>,
    /// Sequence markers in sequence ticks; they do not move with clips (omitted when empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<StudioMarker>,
    /// The work area (omitted when unset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_area: Option<WorkArea>,
}
impl StudioSequence {
    /// End of the last clip over all tracks; `None` on tick overflow.
    pub fn content_end(&self) -> Option<Ticks> {
        let mut end = 0u64;
        for clip in self.tracks.iter().flat_map(|t| t.clips.iter()) {
            end = end.max(clip.timeline_end()?.value());
        }
        Some(Ticks::new(end))
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a.checked_rem(b).unwrap_or(0));
    }
    a
}
fn derived_duration(span: u64, speed: &Speed) -> Option<u64> {
    if !speed.valid() {
        return None;
    }
    let scaled = u128::from(span).checked_mul(u128::from(speed.denominator))?;
    let magnitude = u128::from(speed.numerator.unsigned_abs());
    if scaled.checked_rem(magnitude)? != 0 {
        return None;
    }
    u64::try_from(scaled.checked_div(magnitude)?).ok()
}
pub(crate) fn control_free(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_HANDLE_BYTES && !s.chars().any(char::is_control)
}
/// A handle is an opaque id, never a filesystem path (STU-VID-020): any path separator (so rooted,
/// relative, `./` and `../` spellings), drive prefixes, home (`~`) prefixes and bare `.`/`..` are refused.
pub(crate) fn handle(s: &str) -> bool {
    let drive = s.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && s.as_bytes().get(1) == Some(&b':');
    control_free(s)
        && !s.contains(['/', '\\'])
        && !drive
        && !s.starts_with('~')
        && s != "."
        && s != ".."
}

/// Count of sequences, tracks, clips, markers and transitions, charged against `Budget::nodes`.
pub(crate) fn node_count(d: &StudioDocument) -> usize {
    d.sequences.iter().fold(0usize, |n, s| {
        let clips = s.tracks.iter().fold(0usize, |m, t| {
            t.clips
                .iter()
                .fold(m.saturating_add(t.clips.len()), |k, c| {
                    k.saturating_add(c.markers.len())
                        .saturating_add(usize::from(c.transitions.head.is_some()))
                        .saturating_add(usize::from(c.transitions.tail.is_some()))
                })
        });
        n.saturating_add(1)
            .saturating_add(s.tracks.len())
            .saturating_add(s.markers.len())
            .saturating_add(clips)
    })
}

fn text_ok(s: &str, allow_empty: bool) -> bool {
    (allow_empty || !s.is_empty())
        && s.len() <= MAX_MARKER_TEXT_BYTES
        && !s.chars().any(char::is_control)
}

/// One marker: key id unique in the document, bounded text, `start + duration` in range.
fn validate_marker<'a>(
    m: &'a StudioMarker,
    ids: &mut BTreeSet<&'a str>,
    payload_size: &mut usize,
    b: Budget,
) -> Result<()> {
    let target = m.marker_id.as_str();
    if !key(target) {
        return Err(err(Code::InvalidId, "marker_id"));
    }
    if !ids.insert(target) {
        return Err(err(Code::DuplicateIdentity, target));
    }
    let role_text = match &m.role {
        Some(MarkerRole::WebLink { url }) => Some((url.as_str(), false)),
        Some(MarkerRole::Caption { text }) => Some((text.as_str(), true)),
        _ => None,
    };
    // A comment may hold line breaks; names and role text may not hold control characters.
    let comment_ok = m.comment.len() <= MAX_MARKER_TEXT_BYTES;
    if !text_ok(&m.name, true)
        || !comment_ok
        || role_text.is_some_and(|(s, empty)| !text_ok(s, empty))
    {
        return Err(err(Code::InvalidField, format!("{target}/text")));
    }
    let bytes = m
        .name
        .len()
        .saturating_add(m.comment.len())
        .saturating_add(role_text.map_or(0, |(s, _)| s.len()));
    charge(payload_size, bytes, b.payload_bytes, target)?;
    if m.start.checked_add(m.duration).is_err() {
        return Err(err(Code::InvalidField, format!("{target}/span")));
    }
    Ok(())
}

/// One transition: key id unique in the document, opaque kind key, non-zero duration, a centre
/// offset only with `custom` and never beyond the duration.
fn validate_transition<'a>(
    t: &'a TimelineTransition,
    ids: &mut BTreeSet<&'a str>,
    payload_size: &mut usize,
    b: Budget,
) -> Result<()> {
    let target = t.transition_id.as_str();
    if !key(target) {
        return Err(err(Code::InvalidId, "transition_id"));
    }
    if !ids.insert(target) {
        return Err(err(Code::DuplicateIdentity, target));
    }
    if !key(&t.transition_kind) {
        return Err(err(Code::InvalidField, format!("{target}/transition_kind")));
    }
    charge(
        payload_size,
        t.transition_kind.len(),
        b.payload_bytes,
        target,
    )?;
    let offset_ok = match t.alignment {
        TransitionAlignment::Custom => t.center_offset.unsigned_abs() <= t.duration.value(),
        _ => t.center_offset == 0,
    };
    if t.duration.value() == 0 || !offset_ok {
        return Err(err(Code::InvalidField, format!("{target}/timing")));
    }
    if t.parameters.len() > MAX_TRANSITION_PARAMETERS {
        return Err(err(Code::BudgetExceeded, format!("{target}/parameters")));
    }
    let mut keys = BTreeSet::new();
    for p in &t.parameters {
        let value_ok = match &p.param_value {
            TransitionParameterValue::Number(value) => value.is_finite(),
            TransitionParameterValue::Enumeration(value) => key(value),
            _ => true,
        };
        if !key(&p.param_key) || !keys.insert(p.param_key.as_str()) || !value_ok {
            return Err(err(Code::InvalidField, format!("{target}/parameters")));
        }
        charge(payload_size, p.param_key.len(), b.payload_bytes, target)?;
    }
    Ok(())
}

fn charge(total: &mut usize, bytes: usize, limit: usize, target: &str) -> Result<()> {
    *total = total
        .checked_add(bytes)
        .filter(|n| *n <= limit)
        .ok_or_else(|| err(Code::BudgetExceeded, target))?;
    Ok(())
}

pub(crate) fn validate_sequences(
    d: &StudioDocument,
    b: Budget,
    token: &hsk_studio_accord::CancellationToken,
    r: &dyn Resolver,
    mut payload_size: usize,
    issues: &mut Vec<ResolutionIssue>,
) -> Result<()> {
    if d.sequences.is_empty() {
        return Ok(());
    }
    canceled(token)?;
    if d.sequences.len() > MAX_SEQUENCES {
        return Err(err(Code::BudgetExceeded, "sequences"));
    }
    let mut sequence_ids = BTreeSet::new();
    for s in &d.sequences {
        if !key(&s.sequence_id) {
            return Err(err(Code::InvalidId, "sequence_id"));
        }
        if !sequence_ids.insert(s.sequence_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, &s.sequence_id));
        }
    }
    let (mut track_ids, mut clip_ids) = (BTreeSet::new(), BTreeSet::new());
    let (mut marker_ids, mut transition_ids) = (BTreeSet::new(), BTreeSet::new());
    let mut links = Vec::new();
    for s in &d.sequences {
        canceled(token)?;
        let sid = s.sequence_id.as_str();
        charge(&mut payload_size, s.name.len(), b.payload_bytes, sid)?;
        if s.frame_rate.accord().is_none() {
            return Err(err(Code::InvalidField, format!("{sid}/frame_rate")));
        }
        if s.tracks.len() > MAX_TRACKS_PER_SEQUENCE {
            return Err(err(Code::BudgetExceeded, format!("{sid}/tracks")));
        }
        if let Some(area) = &s.work_area
            && area.start >= area.end
        {
            return Err(err(Code::InvalidField, format!("{sid}/work_area")));
        }
        // Counted before any marker is examined (hostile list lengths, V4-02).
        let markers_here = s
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .fold(s.markers.len(), |n, c| n.saturating_add(c.markers.len()));
        if markers_here > MAX_MARKERS_PER_SEQUENCE {
            return Err(err(Code::BudgetExceeded, format!("{sid}/markers")));
        }
        for m in &s.markers {
            validate_marker(m, &mut marker_ids, &mut payload_size, b)?;
        }
        let (mut stack, mut clips_here, mut content_end) = ([0u32; 4], 0usize, 0u64);
        for t in &s.tracks {
            canceled(token)?;
            id(&t.track_id, "STRK", &t.track_id)?;
            if !track_ids.insert(t.track_id.as_str()) {
                return Err(err(Code::DuplicateIdentity, &t.track_id));
            }
            let slot = &mut stack[t.kind.slot()];
            if t.index != *slot {
                return Err(err(Code::InvalidField, format!("{}/index", t.track_id)));
            }
            let gain_ok = t.volume.is_finite() && (0.0..=MAX_TRACK_VOLUME).contains(&t.volume);
            if !gain_ok || !t.pan.is_finite() || !(-1.0..=1.0).contains(&t.pan) {
                return Err(err(Code::InvalidField, format!("{}/mix", t.track_id)));
            }
            *slot = slot
                .checked_add(1)
                .ok_or_else(|| err(Code::BudgetExceeded, format!("{sid}/tracks")))?;
            charge(
                &mut payload_size,
                t.name.len(),
                b.payload_bytes,
                &t.track_id,
            )?;
            let mut previous: Option<(u64, u64)> = None;
            let mut previous_tail = false;
            for c in &t.clips {
                canceled(token)?;
                clips_here = clips_here.saturating_add(1);
                if clips_here > MAX_CLIPS_PER_SEQUENCE {
                    return Err(err(Code::BudgetExceeded, format!("{sid}/clips")));
                }
                let cid = c.clip_id.as_str();
                id(cid, "SCLP", cid)?;
                if !clip_ids.insert(cid) {
                    return Err(err(Code::DuplicateIdentity, cid));
                }
                if let Some(group) = &c.link_group {
                    if !key(group) {
                        return Err(err(Code::InvalidField, format!("{cid}/link_group")));
                    }
                    charge(&mut payload_size, group.len(), b.payload_bytes, cid)?;
                }
                match &c.source_ref {
                    ClipSource::Media {
                        artifact_manifest_id,
                        content_digest,
                    } => {
                        if !handle(artifact_manifest_id)
                            || !control_free(&content_digest.algorithm)
                            || !control_free(&content_digest.digest)
                        {
                            return Err(err(Code::InvalidField, format!("{cid}/source")));
                        }
                        let bytes = artifact_manifest_id
                            .len()
                            .saturating_add(content_digest.algorithm.len())
                            .saturating_add(content_digest.digest.len());
                        charge(&mut payload_size, bytes, b.payload_bytes, cid)?;
                        let disposition = r.media(artifact_manifest_id, content_digest);
                        if disposition != Resolution::Available {
                            issues.push(ResolutionIssue {
                                target: format!("{cid}/source"),
                                disposition,
                            });
                        }
                    }
                    ClipSource::Sequence { sequence_id } => {
                        if !key(sequence_id) {
                            return Err(err(Code::InvalidId, format!("{cid}/source")));
                        }
                        if !sequence_ids.contains(sequence_id.as_str()) {
                            return Err(err(Code::DanglingReference, format!("{cid}/source")));
                        }
                        links.push((s.sequence_id.clone(), sequence_id.clone()));
                    }
                    ClipSource::Primitive {
                        schema_id,
                        object_id,
                    } => {
                        SchemaId::parse(schema_id)
                            .map_err(|_| err(Code::UnsupportedSchema, format!("{cid}/source")))?;
                        if !handle(object_id) {
                            return Err(err(Code::InvalidField, format!("{cid}/source")));
                        }
                        charge(
                            &mut payload_size,
                            schema_id.len().saturating_add(object_id.len()),
                            b.payload_bytes,
                            cid,
                        )?;
                        let disposition = r.primitive(schema_id, object_id);
                        if disposition != Resolution::Available {
                            issues.push(ResolutionIssue {
                                target: format!("{cid}/source"),
                                disposition,
                            });
                        }
                    }
                }
                if c.source_in.value() >= c.source_out.value() {
                    return Err(err(Code::InvalidField, format!("{cid}/source_window")));
                }
                if !c.speed.valid() {
                    return Err(err(Code::InvalidField, format!("{cid}/speed")));
                }
                if c.derived_duration() != Some(c.duration) {
                    return Err(err(Code::InvalidField, format!("{cid}/duration")));
                }
                let start = c.timeline_start.value();
                let end = c
                    .timeline_end()
                    .ok_or_else(|| err(Code::InvalidField, format!("{cid}/timeline_end")))?
                    .value();
                if let Some((previous_start, previous_end)) = previous {
                    if start < previous_start {
                        return Err(err(Code::InvalidField, format!("{cid}/unordered")));
                    }
                    if start < previous_end {
                        return Err(err(Code::InvalidField, format!("{cid}/overlap")));
                    }
                }
                for m in &c.markers {
                    validate_marker(m, &mut marker_ids, &mut payload_size, b)?;
                }
                for tr in [&c.transitions.head, &c.transitions.tail]
                    .into_iter()
                    .flatten()
                {
                    validate_transition(tr, &mut transition_ids, &mut payload_size, b)?;
                }
                // One transition per cut: the outgoing clip's tail holds it, so an abutting
                // incoming clip's head stays empty.
                if c.transitions.head.is_some()
                    && previous_tail
                    && previous.is_some_and(|(_, previous_end)| previous_end == start)
                {
                    return Err(err(Code::InvalidField, format!("{cid}/transitions")));
                }
                previous_tail = c.transitions.tail.is_some();
                previous = Some((start, end));
                content_end = content_end.max(end);
            }
        }
        if s.duration.value() != content_end {
            return Err(err(Code::InvalidField, format!("{sid}/duration")));
        }
    }
    if !links.is_empty() {
        let nodes: BTreeSet<String> = sequence_ids.iter().map(|s| (*s).to_owned()).collect();
        cycle_check(&nodes, &links, token, Code::ContainmentCycle).map_err(
            |e: Diagnostic| match e.code {
                Code::ContainmentCycle => err(Code::ContainmentCycle, "sequences"),
                _ => e,
            },
        )?;
    }
    Ok(())
}
