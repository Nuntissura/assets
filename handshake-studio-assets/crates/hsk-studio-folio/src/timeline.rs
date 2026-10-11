//! Timeline members of the one `StudioDocument` graph: `StudioSequence`, `StudioTrack`, `StudioClip`
//! (spec STU-VID-010, STU-VID-020, STU-VID-021). Every time value is `hsk_studio_accord::Ticks`
//! (254016000000 per second, STU-VID-012) and is a plain unsigned integer on the wire. Frame-rate
//! conversion, timecode and edit arithmetic belong to Pulse; this module stores and validates only.
//!
//! Declared v02.215 gaps, not invented here: no identity prefix is specified for a sequence, so
//! `sequence_id` is a document-local key (`[A-Za-z0-9_-]{1,128}`); the spec lists both a `reversed`
//! flag and "reverse is a negative speed", so reversal is carried by the speed sign only; the sequence
//! `duration` is not defined by the spec and is stored redundantly as the end of the last clip, like
//! clip `duration`. Settings record, markers, work area, effect stack, transitions, label colour and
//! track height/routing are not stored yet.
use super::{
    Budget, Code, ContentDigest, Diagnostic, Resolution, ResolutionIssue, Resolver, Result,
    StudioDocument, canceled, cycle_check, err, id, key, required,
};
use hsk_studio_accord::{FrameRate as AccordFrameRate, SchemaId, TICKS_PER_SECOND, Ticks};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeSet;

schema_token!(SequenceSchema, "hsk.studio.sequence@1");
schema_token!(TrackSchema, "hsk.studio.track@1");
schema_token!(ClipSchema, "hsk.studio.clip@1");

/// Hard ceilings independent of the caller `Budget` (which also counts every sequence, track and clip
/// against `Budget::nodes`).
pub const MAX_SEQUENCES: usize = 64;
pub const MAX_TRACKS_PER_SEQUENCE: usize = 1024;
pub const MAX_CLIPS_PER_SEQUENCE: usize = 262_144;
/// Longest opaque handle (manifest id, digest part, object id).
pub const MAX_HANDLE_BYTES: usize = 256;

mod ticks_wire {
    use super::{Deserialize, Deserializer, Serializer, Ticks};
    pub(super) fn serialize<S: Serializer>(t: &Ticks, s: S) -> std::result::Result<S::Ok, S::Error> {
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
    /// Exact reduced frames-per-second fraction `(numerator, denominator)`, e.g. 24000/1001.
    pub fn fps_fraction(&self) -> Option<(u64, u64)> {
        if self.ticks_per_frame == 0 {
            return None;
        }
        let g = gcd(TICKS_PER_SECOND, self.ticks_per_frame);
        Some((TICKS_PER_SECOND / g, self.ticks_per_frame / g))
    }
    /// Accord's canonical frame rate when the value is a canonical table entry.
    pub fn accord(&self) -> Option<AccordFrameRate> {
        match AccordFrameRate::from_import(self.ticks_per_frame) {
            Ok((rate, normalization)) if !normalization.changed => Some(rate),
            _ => None,
        }
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
    Primitive { schema_id: String, object_id: String },
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
        (a, b) = (b, a % b);
    }
    a
}
fn derived_duration(span: u64, speed: &Speed) -> Option<u64> {
    if !speed.valid() {
        return None;
    }
    let scaled = u128::from(span).checked_mul(u128::from(speed.denominator))?;
    let magnitude = u128::from(speed.numerator.unsigned_abs());
    if scaled % magnitude != 0 {
        return None;
    }
    u64::try_from(scaled / magnitude).ok()
}
fn control_free(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_HANDLE_BYTES && !s.chars().any(char::is_control)
}
/// A handle is an opaque id; backslashes, rooted paths and drive prefixes are filesystem paths.
fn handle(s: &str) -> bool {
    let drive = s.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && s.as_bytes().get(1) == Some(&b':');
    control_free(s) && !s.contains('\\') && !s.starts_with('/') && !drive
}

/// Count of sequences, tracks and clips, charged against `Budget::nodes`.
pub(crate) fn node_count(d: &StudioDocument) -> usize {
    d.sequences.iter().fold(0usize, |n, s| {
        let clips = s
            .tracks
            .iter()
            .fold(0usize, |m, t| m.saturating_add(t.clips.len()));
        n.saturating_add(1)
            .saturating_add(s.tracks.len())
            .saturating_add(clips)
    })
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
            *slot += 1;
            charge(&mut payload_size, t.name.len(), b.payload_bytes, &t.track_id)?;
            let mut previous: Option<(u64, u64)> = None;
            for c in &t.clips {
                canceled(token)?;
                clips_here += 1;
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
                        let bytes = artifact_manifest_id.len()
                            + content_digest.algorithm.len()
                            + content_digest.digest.len();
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
                            schema_id.len() + object_id.len(),
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
