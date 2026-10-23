//! Header Metadata (ST 377-1 section 9): the Primer Pack, Local Sets, and the structural metadata the
//! reader needs to identify essence tracks: Preface -> Content Storage -> Packages -> Timeline Tracks ->
//! Sequences, File Descriptors (picture / sound), Essence Container Data (BodySID / IndexSID).
//!
//! Local Sets are `tag (2) + length + value` (6.3.5): the length is 2 bytes when the set key's byte 6 is
//! 0x53 and BER when it is 0x13. Tags are dynamic: every property is identified through the Primer's
//! tag -> UL map (9.2), by the last eight bytes of its UL (the registry version byte, byte 8, is ignored
//! like for the Fill item). The property ULs below are those of ST 377-1 Annexes A, B, E and F, checked
//! against the Primer Packs of ffmpeg-written files (`tests/mxf_index.rs`).
//!
//! Hostile input: the metadata region is bounded by `Limits::max_probe_bytes`, the number of sets and of
//! batch members is bounded, references that do not resolve are skipped (a decoder must cope with
//! unresolved references, 6.8), every length is checked, nothing is allocated from an unchecked count.
use super::klv::{KEY_LEN, corrupt, header_set, is_fill, is_primer, parse_klv_head};
use super::{MxfDescriptor, MxfPackageKind, MxfTrack, MxfTrackKind};
use crate::error::ReelError;
use crate::model::Limits;
use std::collections::HashMap;

type Tail = [u8; 8];

// Property ULs, bytes 9..16 (see the module docs).
const INSTANCE_UID: Tail = [0x01, 0x01, 0x15, 0x02, 0, 0, 0, 0];
const CONTENT_STORAGE: Tail = [0x06, 0x01, 0x01, 0x04, 0x02, 0x01, 0, 0];
const OPERATIONAL_PATTERN: Tail = [0x01, 0x02, 0x02, 0x03, 0, 0, 0, 0];
const PACKAGES: Tail = [0x06, 0x01, 0x01, 0x04, 0x05, 0x01, 0, 0];
const ESSENCE_CONTAINER_DATA: Tail = [0x06, 0x01, 0x01, 0x04, 0x05, 0x02, 0, 0];
const LINKED_PACKAGE_UID: Tail = [0x06, 0x01, 0x01, 0x06, 0x01, 0, 0, 0];
const INDEX_SID: Tail = [0x01, 0x03, 0x04, 0x05, 0, 0, 0, 0];
const BODY_SID: Tail = [0x01, 0x03, 0x04, 0x04, 0, 0, 0, 0];
const PACKAGE_UID: Tail = [0x01, 0x01, 0x15, 0x10, 0, 0, 0, 0];
const PACKAGE_TRACKS: Tail = [0x06, 0x01, 0x01, 0x04, 0x06, 0x05, 0, 0];
const SOURCE_DESCRIPTOR: Tail = [0x06, 0x01, 0x01, 0x04, 0x02, 0x03, 0, 0];
const TRACK_ID: Tail = [0x01, 0x07, 0x01, 0x01, 0, 0, 0, 0];
const TRACK_NUMBER: Tail = [0x01, 0x04, 0x01, 0x03, 0, 0, 0, 0];
const TRACK_SEQUENCE: Tail = [0x06, 0x01, 0x01, 0x04, 0x02, 0x04, 0, 0];
const EDIT_RATE: Tail = [0x05, 0x30, 0x04, 0x05, 0, 0, 0, 0];
const ORIGIN: Tail = [0x07, 0x02, 0x01, 0x03, 0x01, 0x03, 0, 0];
const DATA_DEFINITION: Tail = [0x04, 0x07, 0x01, 0, 0, 0, 0, 0];
const SEQUENCE_DURATION: Tail = [0x07, 0x02, 0x02, 0x01, 0x01, 0x03, 0, 0];
const STRUCTURAL_COMPONENTS: Tail = [0x06, 0x01, 0x01, 0x04, 0x06, 0x09, 0, 0];
const SOURCE_PACKAGE_ID: Tail = [0x06, 0x01, 0x01, 0x03, 0x01, 0, 0, 0];
const SOURCE_TRACK_ID: Tail = [0x06, 0x01, 0x01, 0x03, 0x02, 0, 0, 0];
const LINKED_TRACK_ID: Tail = [0x06, 0x01, 0x01, 0x03, 0x05, 0, 0, 0];
const SAMPLE_RATE: Tail = [0x04, 0x06, 0x01, 0x01, 0, 0, 0, 0];
const CONTAINER_DURATION: Tail = [0x04, 0x06, 0x01, 0x02, 0, 0, 0, 0];
const ESSENCE_CONTAINER: Tail = [0x06, 0x01, 0x01, 0x04, 0x01, 0x02, 0, 0];
const SUB_DESCRIPTORS: Tail = [0x06, 0x01, 0x01, 0x04, 0x06, 0x0B, 0, 0];
const FRAME_LAYOUT: Tail = [0x04, 0x01, 0x03, 0x01, 0x04, 0, 0, 0];
const STORED_WIDTH: Tail = [0x04, 0x01, 0x05, 0x02, 0x02, 0, 0, 0];
const STORED_HEIGHT: Tail = [0x04, 0x01, 0x05, 0x02, 0x01, 0, 0, 0];
const SAMPLED_WIDTH: Tail = [0x04, 0x01, 0x05, 0x01, 0x08, 0, 0, 0];
const SAMPLED_HEIGHT: Tail = [0x04, 0x01, 0x05, 0x01, 0x07, 0, 0, 0];
const ASPECT_RATIO: Tail = [0x04, 0x01, 0x01, 0x01, 0x01, 0, 0, 0];
const PICTURE_CODING: Tail = [0x04, 0x01, 0x06, 0x01, 0, 0, 0, 0];
const COMPONENT_DEPTH: Tail = [0x04, 0x01, 0x05, 0x03, 0x0A, 0, 0, 0];
const H_SUBSAMPLING: Tail = [0x04, 0x01, 0x05, 0x01, 0x05, 0, 0, 0];
const V_SUBSAMPLING: Tail = [0x04, 0x01, 0x05, 0x01, 0x10, 0, 0, 0];
const AUDIO_SAMPLING_RATE: Tail = [0x04, 0x02, 0x03, 0x01, 0x01, 0x01, 0, 0];
const LOCKED: Tail = [0x04, 0x02, 0x03, 0x01, 0x04, 0, 0, 0];
const CHANNEL_COUNT: Tail = [0x04, 0x02, 0x01, 0x01, 0x04, 0, 0, 0];
const QUANTIZATION_BITS: Tail = [0x04, 0x02, 0x03, 0x03, 0x04, 0, 0, 0];
const SOUND_CODING: Tail = [0x04, 0x02, 0x04, 0x02, 0, 0, 0, 0];
const BLOCK_ALIGN: Tail = [0x04, 0x02, 0x03, 0x02, 0x01, 0, 0, 0];
const AVG_BPS: Tail = [0x04, 0x02, 0x03, 0x03, 0x05, 0, 0, 0];

const WANTED: &[Tail] = &[
    INSTANCE_UID,
    CONTENT_STORAGE,
    OPERATIONAL_PATTERN,
    PACKAGES,
    ESSENCE_CONTAINER_DATA,
    LINKED_PACKAGE_UID,
    INDEX_SID,
    BODY_SID,
    PACKAGE_UID,
    PACKAGE_TRACKS,
    SOURCE_DESCRIPTOR,
    TRACK_ID,
    TRACK_NUMBER,
    TRACK_SEQUENCE,
    EDIT_RATE,
    ORIGIN,
    DATA_DEFINITION,
    SEQUENCE_DURATION,
    STRUCTURAL_COMPONENTS,
    SOURCE_PACKAGE_ID,
    SOURCE_TRACK_ID,
    LINKED_TRACK_ID,
    SAMPLE_RATE,
    CONTAINER_DURATION,
    ESSENCE_CONTAINER,
    SUB_DESCRIPTORS,
    FRAME_LAYOUT,
    STORED_WIDTH,
    STORED_HEIGHT,
    SAMPLED_WIDTH,
    SAMPLED_HEIGHT,
    ASPECT_RATIO,
    PICTURE_CODING,
    COMPONENT_DEPTH,
    H_SUBSAMPLING,
    V_SUBSAMPLING,
    AUDIO_SAMPLING_RATE,
    LOCKED,
    CHANNEL_COUNT,
    QUANTIZATION_BITS,
    SOUND_CODING,
    BLOCK_ALIGN,
    AVG_BPS,
];

// Set kinds (key byte 15, Table 17 and the essence mapping documents).
const KIND_PREFACE: u8 = 0x2F;
const KIND_CONTENT_STORAGE: u8 = 0x18;
const KIND_ESSENCE_CONTAINER_DATA: u8 = 0x23;
const KIND_MATERIAL_PACKAGE: u8 = 0x36;
const KIND_SOURCE_PACKAGE: u8 = 0x37;
const KIND_TIMELINE_TRACK: u8 = 0x3B;
const KIND_SEQUENCE: u8 = 0x0F;
const KIND_SOURCE_CLIP: u8 = 0x11;
const KIND_MULTIPLE_DESCRIPTOR: u8 = 0x44;

const MAX_PRIMER_ENTRIES: usize = 1 << 16;
const MAX_SETS: usize = 1 << 18;
const MAX_BATCH: usize = 1 << 16;
const MAX_VALUE_KEPT: usize = 4096;
/// Properties that hold batches of references and may be longer than `MAX_VALUE_KEPT`.
const BATCH_TAILS: &[Tail] = &[
    PACKAGES,
    ESSENCE_CONTAINER_DATA,
    PACKAGE_TRACKS,
    STRUCTURAL_COMPONENTS,
    SUB_DESCRIPTORS,
];

struct RawSet {
    kind: u8,
    values: Vec<(Tail, Vec<u8>)>,
}

impl RawSet {
    fn get(&self, tail: &Tail) -> Option<&[u8]> {
        self.values
            .iter()
            .find(|(t, _)| t == tail)
            .map(|(_, v)| v.as_slice())
    }
    fn u32(&self, tail: &Tail) -> Option<u32> {
        self.get(tail)?.try_into().ok().map(u32::from_be_bytes)
    }
    fn i64(&self, tail: &Tail) -> Option<i64> {
        self.get(tail)?.try_into().ok().map(i64::from_be_bytes)
    }
    fn rational(&self, tail: &Tail) -> Option<(i32, i32)> {
        let v: [u8; 8] = self.get(tail)?.try_into().ok()?;
        Some((
            i32::from_be_bytes([v[0], v[1], v[2], v[3]]),
            i32::from_be_bytes([v[4], v[5], v[6], v[7]]),
        ))
    }
    fn label(&self, tail: &Tail) -> Option<[u8; KEY_LEN]> {
        self.get(tail)?.try_into().ok()
    }
    fn refs(&self, tail: &Tail) -> Vec<[u8; KEY_LEN]> {
        self.get(tail).map(batch).unwrap_or_default()
    }
}

/// A Batch / Array of 16-byte items: `count u32, item length u32 (16), items`. Malformed -> empty.
fn batch(value: &[u8]) -> Vec<[u8; KEY_LEN]> {
    let (Some(count), Some(item)) = (
        value.get(..4).and_then(|b| <[u8; 4]>::try_from(b).ok()),
        value.get(4..8).and_then(|b| <[u8; 4]>::try_from(b).ok()),
    ) else {
        return Vec::new();
    };
    let (count, item) = (u32::from_be_bytes(count) as usize, u32::from_be_bytes(item));
    let body = value.get(8..).unwrap_or_default();
    if item != KEY_LEN as u32
        || count > MAX_BATCH
        || count.checked_mul(KEY_LEN).is_none_or(|n| n > body.len())
    {
        return Vec::new();
    }
    body.chunks_exact(KEY_LEN)
        .take(count)
        .filter_map(|c| <[u8; KEY_LEN]>::try_from(c).ok())
        .collect()
}

/// Registry-independent identity of a property UL: the last eight bytes, for a SMPTE `01.01.01.xx` UL.
fn tail_of(ul: &[u8; KEY_LEN]) -> Option<Tail> {
    (ul[..4] == [0x06, 0x0E, 0x2B, 0x34] && ul[4..7] == [0x01, 0x01, 0x01])
        .then(|| <Tail>::try_from(&ul[8..]).ok())
        .flatten()
}

fn parse_primer(value: &[u8]) -> Result<HashMap<u16, Tail>, ReelError> {
    let count = value
        .get(..4)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map(u32::from_be_bytes)
        .ok_or_else(|| corrupt("mxf-primer"))? as usize;
    let item = value
        .get(4..8)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map(u32::from_be_bytes)
        .ok_or_else(|| corrupt("mxf-primer"))?;
    let body = value.get(8..).unwrap_or_default();
    if item != 18
        || count > MAX_PRIMER_ENTRIES
        || count.checked_mul(18).is_none_or(|n| n > body.len())
    {
        return Err(corrupt("mxf-primer"));
    }
    let mut map = HashMap::with_capacity(count);
    for entry in body.chunks_exact(18).take(count) {
        let tag = u16::from_be_bytes([entry[0], entry[1]]);
        let ul: [u8; KEY_LEN] = entry[2..18].try_into().map_err(|_| corrupt("mxf-primer"))?;
        if let Some(tail) = tail_of(&ul) {
            map.insert(tag, tail);
        }
    }
    Ok(map)
}

/// Local set `value` into the wanted properties.
fn parse_local_set(
    kind: u8,
    ber_lengths: bool,
    value: &[u8],
    primer: &HashMap<u16, Tail>,
) -> Result<(Option<[u8; KEY_LEN]>, RawSet), ReelError> {
    let (mut at, mut instance) = (0usize, None);
    let mut set = RawSet {
        kind,
        values: Vec::new(),
    };
    while at < value.len() {
        let tag = value
            .get(at..at.saturating_add(2))
            .and_then(|b| <[u8; 2]>::try_from(b).ok())
            .map(u16::from_be_bytes)
            .ok_or_else(|| corrupt("mxf-local-tag"))?;
        at = at.saturating_add(2);
        let len = if ber_lengths {
            let head = value
                .get(at..)
                .filter(|rest| !rest.is_empty())
                .ok_or_else(|| corrupt("mxf-local-length"))?;
            let first = head[0];
            if first < 0x80 {
                at = at.saturating_add(1);
                usize::from(first)
            } else {
                let n = usize::from(first & 0x7F);
                let octets = head
                    .get(1..1usize.saturating_add(n))
                    .filter(|_| (1..=8).contains(&n))
                    .ok_or_else(|| corrupt("mxf-local-length"))?;
                at = at.saturating_add(1).saturating_add(n);
                usize::try_from(octets.iter().fold(0u64, |a, b| (a << 8) | u64::from(*b)))
                    .map_err(|_| corrupt("mxf-local-length"))?
            }
        } else {
            let l = value
                .get(at..at.saturating_add(2))
                .and_then(|b| <[u8; 2]>::try_from(b).ok())
                .map(u16::from_be_bytes)
                .ok_or_else(|| corrupt("mxf-local-length"))?;
            at = at.saturating_add(2);
            usize::from(l)
        };
        let body = value
            .get(at..at.saturating_add(len))
            .ok_or_else(|| corrupt("mxf-local-length"))?;
        at = at.saturating_add(len);
        // 3C.0A is the Instance UID by static tag (Annex H); everything else needs the Primer.
        let tail = if tag == 0x3C0A {
            Some(INSTANCE_UID)
        } else {
            primer.get(&tag).copied()
        };
        let Some(tail) = tail.filter(|t| WANTED.contains(t)) else {
            continue;
        };
        if tail == INSTANCE_UID {
            instance = <[u8; KEY_LEN]>::try_from(body).ok();
        }
        if body.len() <= MAX_VALUE_KEPT || BATCH_TAILS.contains(&tail) {
            set.values.push((tail, body.to_vec()));
        }
    }
    Ok((instance, set))
}

/// Everything resolved from one Header Metadata region.
pub(super) struct Resolved {
    pub operational_pattern: Option<[u8; KEY_LEN]>,
    pub tracks: Vec<MxfTrack>,
}

/// Parse and resolve a Header Metadata region (`region` starts at the Primer Pack key).
pub(super) fn resolve(region: &[u8], limits: &Limits) -> Result<Resolved, ReelError> {
    let mut at = 0usize;
    let mut primer: Option<HashMap<u16, Tail>> = None;
    let mut sets: HashMap<[u8; KEY_LEN], RawSet> = HashMap::new();
    let mut order: Vec<[u8; KEY_LEN]> = Vec::new();
    let max_sets = MAX_SETS.min(limits.max_samples.max(1024));
    while region.len().saturating_sub(at) > KEY_LEN {
        let klv = parse_klv_head(&region[at..], at as u64)?;
        let value_at = usize::try_from(klv.value_pos()).map_err(|_| corrupt("mxf-klv-offset"))?;
        let value_len = usize::try_from(klv.value_len).map_err(|_| corrupt("mxf-klv-length"))?;
        let end = value_at
            .checked_add(value_len)
            .filter(|end| *end <= region.len())
            .ok_or_else(|| corrupt("klv-length-exceeds-source"))?;
        let value = &region[value_at..end];
        at = end;
        if is_fill(&klv.key) {
            continue;
        }
        if is_primer(&klv.key) {
            primer = Some(parse_primer(value)?);
            continue;
        }
        let Some((kind, ber)) = header_set(&klv.key) else {
            continue; // dark metadata (6.3.2): skipped by its length
        };
        let Some(primer) = &primer else {
            return Err(corrupt("mxf-metadata-before-primer"));
        };
        if sets.len() >= max_sets {
            return Err(ReelError::LimitExceeded {
                what: "mxf-metadata-sets",
            });
        }
        let (instance, set) = parse_local_set(kind, ber, value, primer)?;
        if let Some(id) = instance {
            order.push(id);
            sets.insert(id, set);
        }
    }
    if primer.is_none() {
        return Err(corrupt("mxf-primer-missing"));
    }
    Ok(link(&sets, &order))
}

fn classify(data_definition: Option<[u8; KEY_LEN]>) -> MxfTrackKind {
    // SMPTE ST 400 data definition labels (not part of ST 377-1): ...01.03.02.02.01 picture, .02 sound,
    // .03 data; ...01.03.02.01.xx timecode. Verified against ffmpeg-written files; any other label
    // is `Other` (raw label kept on the track).
    let Some(dd) = data_definition else {
        return MxfTrackKind::Other;
    };
    match dd[8..13] {
        [0x01, 0x03, 0x02, 0x02, 0x01] => MxfTrackKind::Picture,
        [0x01, 0x03, 0x02, 0x02, 0x02] => MxfTrackKind::Sound,
        [0x01, 0x03, 0x02, 0x02, 0x03] => MxfTrackKind::Data,
        [0x01, 0x03, 0x02, 0x01, _] => MxfTrackKind::Timecode,
        _ => MxfTrackKind::Other,
    }
}

fn descriptor_from(set: &RawSet) -> MxfDescriptor {
    MxfDescriptor {
        set_kind: set.kind,
        linked_track_id: set.u32(&LINKED_TRACK_ID),
        sample_rate: set.rational(&SAMPLE_RATE),
        container_duration: set.i64(&CONTAINER_DURATION),
        essence_container: set.label(&ESSENCE_CONTAINER),
        frame_layout: set.get(&FRAME_LAYOUT).and_then(|v| v.first().copied()),
        stored_width: set.u32(&STORED_WIDTH),
        stored_height: set.u32(&STORED_HEIGHT),
        sampled_width: set.u32(&SAMPLED_WIDTH),
        sampled_height: set.u32(&SAMPLED_HEIGHT),
        aspect_ratio: set.rational(&ASPECT_RATIO),
        picture_essence_coding: set.label(&PICTURE_CODING),
        component_depth: set.u32(&COMPONENT_DEPTH),
        horizontal_subsampling: set.u32(&H_SUBSAMPLING),
        vertical_subsampling: set.u32(&V_SUBSAMPLING),
        audio_sampling_rate: set.rational(&AUDIO_SAMPLING_RATE),
        locked: set.get(&LOCKED).and_then(|v| v.first().map(|b| *b != 0)),
        channel_count: set.u32(&CHANNEL_COUNT),
        quantization_bits: set.u32(&QUANTIZATION_BITS),
        sound_essence_coding: set.label(&SOUND_CODING),
        block_align: set
            .get(&BLOCK_ALIGN)
            .and_then(|v| <[u8; 2]>::try_from(v).ok())
            .map(u16::from_be_bytes),
        average_bytes_per_second: set.u32(&AVG_BPS),
    }
}

/// Resolve Preface -> Content Storage -> Packages -> Tracks / Descriptors / Essence Container Data.
fn link(sets: &HashMap<[u8; KEY_LEN], RawSet>, order: &[[u8; KEY_LEN]]) -> Resolved {
    let preface = order
        .iter()
        .filter_map(|id| sets.get(id))
        .find(|s| s.kind == KIND_PREFACE);
    let operational_pattern = preface.and_then(|p| p.label(&OPERATIONAL_PATTERN));
    let storage = preface
        .and_then(|p| p.label(&CONTENT_STORAGE))
        .and_then(|id| sets.get(&id))
        .filter(|s| s.kind == KIND_CONTENT_STORAGE);
    let package_ids: Vec<[u8; KEY_LEN]> = match storage {
        Some(storage) => storage.refs(&PACKAGES),
        None => order
            .iter()
            .filter(|id| {
                sets.get(*id)
                    .is_some_and(|s| matches!(s.kind, KIND_MATERIAL_PACKAGE | KIND_SOURCE_PACKAGE))
            })
            .copied()
            .collect(),
    };
    // Essence Container Data: package UMID (32 bytes) -> (BodySID, IndexSID).
    let mut container_data: HashMap<Vec<u8>, (u32, u32)> = HashMap::new();
    for set in order
        .iter()
        .filter_map(|id| sets.get(id))
        .filter(|s| s.kind == KIND_ESSENCE_CONTAINER_DATA)
    {
        if let Some(umid) = set.get(&LINKED_PACKAGE_UID) {
            container_data.insert(
                umid.to_vec(),
                (
                    set.u32(&BODY_SID).unwrap_or(0),
                    set.u32(&INDEX_SID).unwrap_or(0),
                ),
            );
        }
    }
    let mut tracks = Vec::new();
    for package_id in package_ids {
        let Some(package) = sets.get(&package_id) else {
            continue;
        };
        let package_kind = match package.kind {
            KIND_MATERIAL_PACKAGE => MxfPackageKind::Material,
            KIND_SOURCE_PACKAGE => MxfPackageKind::Source,
            _ => continue,
        };
        let umid = package.get(&PACKAGE_UID).map(<[u8]>::to_vec);
        let (body_sid, index_sid) = umid
            .as_ref()
            .and_then(|u| container_data.get(u))
            .copied()
            .unwrap_or((0, 0));
        // Descriptors of a source package: a Multiple Descriptor lists one per track.
        let descriptor_set = package
            .label(&SOURCE_DESCRIPTOR)
            .and_then(|id| sets.get(&id));
        let descriptors: Vec<&RawSet> = match descriptor_set {
            Some(d) if d.kind == KIND_MULTIPLE_DESCRIPTOR => d
                .refs(&SUB_DESCRIPTORS)
                .iter()
                .filter_map(|id| sets.get(id))
                .collect(),
            Some(d) => vec![d],
            None => Vec::new(),
        };
        let first_track = tracks.len();
        for track_id in package.refs(&PACKAGE_TRACKS) {
            let Some(track) = sets
                .get(&track_id)
                .filter(|s| s.kind == KIND_TIMELINE_TRACK)
            else {
                continue;
            };
            let sequence = track
                .label(&TRACK_SEQUENCE)
                .and_then(|id| sets.get(&id))
                .filter(|s| s.kind == KIND_SEQUENCE);
            let data_definition = sequence.and_then(|s| s.label(&DATA_DEFINITION));
            let clip = sequence.and_then(|s| {
                s.refs(&STRUCTURAL_COMPONENTS)
                    .iter()
                    .filter_map(|id| sets.get(id))
                    .find(|c| c.kind == KIND_SOURCE_CLIP)
            });
            tracks.push(MxfTrack {
                package_kind,
                package_uid: umid.clone().and_then(|u| <[u8; 32]>::try_from(u).ok()),
                track_id: track.u32(&TRACK_ID).unwrap_or(0),
                track_number: track.u32(&TRACK_NUMBER).unwrap_or(0),
                kind: classify(data_definition),
                data_definition,
                edit_rate: track.rational(&EDIT_RATE),
                origin: track.i64(&ORIGIN),
                duration: sequence.and_then(|s| s.i64(&SEQUENCE_DURATION)),
                source_package: clip
                    .and_then(|c| c.get(&SOURCE_PACKAGE_ID))
                    .and_then(|u| <[u8; 32]>::try_from(u).ok()),
                source_track_id: clip.and_then(|c| c.u32(&SOURCE_TRACK_ID)),
                body_sid: (body_sid != 0).then_some(body_sid),
                index_sid: (index_sid != 0).then_some(index_sid),
                descriptor: None,
            });
        }
        // Attach descriptors: by LinkedTrackID; a lone descriptor without a link describes the only
        // essence (non-timecode) track of the package.
        let own = tracks.get_mut(first_track..).unwrap_or_default();
        for d in &descriptors {
            let desc = descriptor_from(d);
            let target = match desc.linked_track_id {
                Some(id) => own.iter_mut().find(|t| t.track_id == id),
                None if descriptors.len() == 1 => own
                    .iter_mut()
                    .find(|t| !matches!(t.kind, MxfTrackKind::Timecode)),
                None => None,
            };
            if let Some(track) = target {
                track.descriptor = Some(desc);
            }
        }
    }
    Resolved {
        operational_pattern,
        tracks,
    }
}
