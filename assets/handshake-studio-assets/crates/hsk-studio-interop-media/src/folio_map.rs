//! Maps a neutral [`TimelineProposal`] into a Folio `StudioSequence` and proposes a document
//! revision that Folio itself validates (`validate_document`). Pulls no authority: the caller
//! owns commit. Identities are never minted here: ids come from a caller-supplied [`IdSource`],
//! media identity from a caller-supplied reel map; an unmapped reel becomes an explicit offline
//! reference with a `Missing` loss (STU-VID-038: offline is a first-class state).
use crate::error::{ErrorKind, InteropError, Result};
use crate::model::{Disposition, Lane, LaneKind, LossNote, TimelineProposal};
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{
    Budget, ClipSchema, ClipSource, Code, ContentDigest, Diagnostic, Resolver, SequenceFrameRate,
    SequenceSchema, Snapshot, StudioClip, StudioSequence, StudioTrack, TrackKind, TrackSchema,
    validate_document,
};
use std::collections::{BTreeMap, BTreeSet};

/// Supplies Folio identities (`SCLP-{uuid_v7}`, `STRK-{uuid_v7}`).
pub trait IdSource {
    /// `part` is 0 for the first clip of an event and 1 for the second clip of a `B` event.
    fn clip_id(&mut self, event: u32, part: u32) -> String;
    fn track_id(&mut self, kind: LaneKind, index: u32) -> String;
}

/// Deterministic synthetic ids in the `uuid_v7` shape (not time-ordered); the same proposal and
/// namespace always yield the same ids. A host should rewrite them on adoption (STU-IO-190).
pub struct SequentialIds {
    namespace: u32,
}

impl SequentialIds {
    pub fn new(namespace: u32) -> Self {
        Self { namespace }
    }
}

impl IdSource for SequentialIds {
    fn clip_id(&mut self, event: u32, part: u32) -> String {
        format!(
            "SCLP-{:08x}-0000-7000-8000-{:012x}",
            self.namespace,
            u64::from(event) * 16 + u64::from(part)
        )
    }
    fn track_id(&mut self, kind: LaneKind, index: u32) -> String {
        let kind = u64::from(kind == LaneKind::Audio);
        format!(
            "STRK-{:08x}-0001-7000-8000-{:012x}",
            self.namespace,
            kind * 65_536 + u64::from(index)
        )
    }
}

pub struct SequenceOptions<'a> {
    /// Folio document-local key (`[A-Za-z0-9_-]{1,128}`).
    pub sequence_id: String,
    pub name: String,
    /// Resolved media per reel name; an absent reel becomes an offline reference.
    pub reels: &'a BTreeMap<String, ClipSource>,
}

#[derive(Clone, Debug)]
pub struct BuiltSequence {
    pub sequence: StudioSequence,
    /// Proposal losses plus the ones found while mapping.
    pub losses: Vec<LossNote>,
}

fn sanitize(reel: &str) -> String {
    reel.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn folio_kind(kind: LaneKind) -> TrackKind {
    match kind {
        LaneKind::Video => TrackKind::Video,
        LaneKind::Audio => TrackKind::Audio,
    }
}

pub fn build_sequence(
    proposal: &TimelineProposal,
    options: &SequenceOptions,
    ids: &mut dyn IdSource,
) -> Result<BuiltSequence> {
    let mut losses = proposal.losses.clone();
    let mut offline = BTreeSet::new();
    let mut parts: BTreeMap<u32, u32> = BTreeMap::new();
    let lane_count = |kind: LaneKind| {
        proposal
            .clips
            .iter()
            .filter(|c| c.lane.kind == kind)
            .map(|c| c.lane.index + 1)
            .max()
            .unwrap_or(0)
    };
    // Clip ids are assigned in proposal order so they do not depend on track layout.
    let mut clip_ids = Vec::with_capacity(proposal.clips.len());
    for clip in &proposal.clips {
        let part = parts.entry(clip.event).or_insert(0);
        clip_ids.push(ids.clip_id(clip.event, *part));
        *part += 1;
    }
    let mut tracks = Vec::new();
    let mut end = 0u64;
    for kind in [LaneKind::Video, LaneKind::Audio] {
        for index in 0..lane_count(kind) {
            let lane = Lane { kind, index };
            let mut members: Vec<usize> = (0..proposal.clips.len())
                .filter(|i| proposal.clips[*i].lane == lane)
                .collect();
            members.sort_by_key(|i| (proposal.clips[*i].record_in, *i));
            let mut clips = Vec::with_capacity(members.len());
            for i in members {
                let clip = &proposal.clips[i];
                let source_ref = match options.reels.get(&clip.reel) {
                    Some(source) => source.clone(),
                    None => {
                        let safe = sanitize(&clip.reel);
                        if offline.insert(clip.reel.clone()) {
                            losses.push(LossNote::new(
                                format!("reel {}", clip.reel),
                                "source_ref",
                                Disposition::Missing,
                                "offline_placeholder",
                            ));
                            if safe != clip.reel {
                                losses.push(LossNote::new(
                                    format!("reel {}", clip.reel),
                                    "source_ref",
                                    Disposition::Translated,
                                    "reel_name_sanitized",
                                ));
                            }
                        }
                        ClipSource::Media {
                            artifact_manifest_id: format!("offline-reel-{safe}"),
                            content_digest: ContentDigest {
                                algorithm: "reel-name".into(),
                                digest: safe,
                            },
                            asset_id: None,
                        }
                    }
                };
                let duration = clip
                    .record_out
                    .checked_sub(clip.record_in)
                    .map_err(|_| InteropError::new(ErrorKind::InvalidRange))?;
                end = end.max(clip.record_out.value());
                clips.push(StudioClip {
                    schema_id: ClipSchema::V1,
                    clip_id: clip_ids[i].clone(),
                    source_ref,
                    source_in: clip.source_in,
                    source_out: clip.source_out,
                    timeline_start: clip.record_in,
                    duration,
                    speed: clip.speed,
                    enabled: true,
                    link_group: clip.link.map(|event| format!("edl-link-{event}")),
                    markers: Vec::new(),
                    transitions: hsk_studio_folio::ClipTransitions::default(),
                });
            }
            tracks.push(StudioTrack {
                schema_id: TrackSchema::V1,
                track_id: ids.track_id(kind, index),
                kind: folio_kind(kind),
                index,
                name: format!(
                    "{}{}",
                    if kind == LaneKind::Video { "V" } else { "A" },
                    index + 1
                ),
                enabled: true,
                locked: false,
                sync_locked: true,
                targeted: true,
                muted: false,
                soloed: false,
                volume: 1.0,
                pan: 0.0,
                clips,
            });
        }
    }
    Ok(BuiltSequence {
        sequence: StudioSequence {
            schema_id: SequenceSchema::V1,
            sequence_id: options.sequence_id.clone(),
            name: options.name.clone(),
            frame_rate: SequenceFrameRate {
                ticks_per_frame: proposal.frame_rate.ticks_per_frame(),
            },
            duration: Ticks::new(end),
            tracks,
            markers: Vec::new(),
            work_area: None,
        },
        losses,
    })
}

/// Caller-owned bounded context for Folio validation.
pub struct FolioContext<'a> {
    pub budget: Budget,
    pub resolver: &'a dyn Resolver,
    pub cancel: &'a CancellationToken,
}

/// A Folio-validated prospective document at `base + 1`; nothing is committed.
#[derive(Clone, Debug)]
pub struct DocumentProposal {
    pub snapshot: Snapshot,
    pub base_revision: u64,
    pub target_revision: u64,
    /// True when an existing sequence with the same id was replaced.
    pub replaced: bool,
    pub losses: Vec<LossNote>,
}

fn from_diagnostic(diagnostic: Diagnostic) -> InteropError {
    InteropError::new(match diagnostic.code {
        Code::Canceled => ErrorKind::Canceled,
        Code::BudgetExceeded => ErrorKind::LimitExceeded,
        Code::StaleRevision => ErrorKind::StaleRevision,
        _ => ErrorKind::FolioRejected,
    })
}

/// Inserts or replaces the sequence and lets Folio validate the whole document.
pub fn propose_document_revision(
    base: &Snapshot,
    expected_revision: u64,
    built: BuiltSequence,
    ctx: &FolioContext,
) -> Result<DocumentProposal> {
    ctx.cancel.check()?;
    let document = base.document();
    if document.revision != expected_revision {
        return Err(InteropError::new(ErrorKind::StaleRevision));
    }
    let target_revision = expected_revision
        .checked_add(1)
        .ok_or(InteropError::new(ErrorKind::LimitExceeded))?;
    let mut prospective = document.clone();
    let existing = prospective
        .sequences
        .iter()
        .position(|s| s.sequence_id == built.sequence.sequence_id);
    match existing {
        Some(i) => prospective.sequences[i] = built.sequence,
        None => prospective.sequences.push(built.sequence),
    }
    prospective.revision = target_revision;
    let snapshot = validate_document(prospective, ctx.budget, ctx.cancel, ctx.resolver)
        .map_err(from_diagnostic)?;
    Ok(DocumentProposal {
        snapshot,
        base_revision: expected_revision,
        target_revision,
        replaced: existing.is_some(),
        losses: built.losses,
    })
}
