//! MXF (SMPTE ST 377-1:2019) index reader for OP1a and OP-Atom files: KLV, partition packs, the Primer and
//! Header Metadata (enough to identify essence tracks and read their picture / sound descriptors and edit
//! rate), Index Table Segments with constant and variable edit unit sizes, and frame-accurate byte offsets
//! for every edit unit and every essence element.
//!
//! Authority: SMPTE ST 377-1:2019 (published at pub.smpte.org/pub/st377-1/st377-1-2019.pdf; sections 6.3
//! KLV coding, 6.4.1 KAG, 6.6 run-in, 7 partitions, 9 header metadata, 11 index tables, Annexes A, B, E, F);
//! ST 378 (OP1a) and ST 390 (OP-Atom) only constrain what ST 377-1 allows. UNVERIFIED against their texts:
//! the OP1a / OP-Atom operational pattern labels (the reader accepts any pattern and reports its label)
//! and the ST 400 data definition labels used to classify tracks (see `meta::classify`); both were checked
//! only against ffmpeg-written files.
//!
//! Essence is opaque: nothing here decodes picture or sound. `MxfProvider` delivers essence elements as
//! packets; a decoded request is `CodecUnavailable` naming the essence coding (no decode is attempted).
//!
//! Every length and offset comes from the file: arithmetic is checked (V4-01), allocations come from
//! bounded counts, a BER length that reaches past the source is a typed `Corrupt`.
#![deny(clippy::arithmetic_side_effects)]
mod klv;
mod meta;
mod provider;
mod tables;

use crate::error::{ReelError, check_cancel};
use crate::model::{Limits, ReadSeek};
use hsk_studio_accord::CancellationToken;
use klv::{
    absolute, corrupt, essence_element, find_header_partition, generic_container_item, is_fill,
    partition_pack, read_at, read_klv, read_partition,
};
use std::io::{Read, Seek, SeekFrom};

pub use provider::MxfProvider;

pub const MXF_PROVIDER_ID: &str = "mxf";
const DECODER_VERSION: &str = "1";
const MAX_PARTITIONS: usize = 1 << 16;
const MAX_FILLS: usize = 64;
/// Most KLVs one edit unit of stored essence may hold (picture, sound, data, system items, fills).
const MAX_ELEMENTS_PER_UNIT: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MxfPartitionKind {
    Header,
    Body,
    Footer,
}

/// A KLV found at the start of a partition's essence (kept to size clip-wrapped essence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MxfKlvInfo {
    pub key: [u8; 16],
    pub header_len: u64,
    pub value_len: u64,
}

/// One partition (7.1). Offsets named `pack_end`..`end` are absolute file offsets; the stored offsets
/// (`this_partition`, `previous_partition`, `footer_partition`) are relative to the header partition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MxfPartition {
    pub kind: MxfPartitionKind,
    /// Byte 15 of the key: 1 open+incomplete, 2 closed+incomplete, 3 open+complete, 4 closed+complete.
    pub status: u8,
    /// Absolute file offset of the pack key.
    pub offset: u64,
    pub major_version: u16,
    pub minor_version: u16,
    pub kag_size: u32,
    pub this_partition: u64,
    pub previous_partition: u64,
    pub footer_partition: u64,
    pub header_byte_count: u64,
    pub index_byte_count: u64,
    pub index_sid: u32,
    pub body_offset: u64,
    pub body_sid: u32,
    pub operational_pattern: [u8; 16],
    pub essence_containers: Vec<[u8; 16]>,
    pub pack_end: u64,
    /// First byte of the Header Metadata region (the Primer key) when `header_byte_count > 0`.
    pub header_start: u64,
    pub index_start: u64,
    /// First byte of the first essence key (after Header Metadata, index and KAG fill); `None` without
    /// essence in this partition.
    pub essence_start: Option<u64>,
    pub essence_first_klv: Option<MxfKlvInfo>,
    /// Start of the next partition, or the end of the source.
    pub end: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MxfPackageKind {
    Material,
    Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MxfTrackKind {
    Picture,
    Sound,
    Data,
    Timecode,
    Other,
}

/// Picture / sound / file descriptor facts (Annex F), as stored. Absent properties are `None`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MxfDescriptor {
    /// Byte 15 of the descriptor set key (0x28 CDCI, 0x29 RGBA, 0x47 AES3, 0x48 Wave, 0x51 MPEG video, ...).
    pub set_kind: u8,
    pub linked_track_id: Option<u32>,
    pub sample_rate: Option<(i32, i32)>,
    pub container_duration: Option<i64>,
    pub essence_container: Option<[u8; 16]>,
    pub frame_layout: Option<u8>,
    pub stored_width: Option<u32>,
    pub stored_height: Option<u32>,
    pub sampled_width: Option<u32>,
    pub sampled_height: Option<u32>,
    pub aspect_ratio: Option<(i32, i32)>,
    pub picture_essence_coding: Option<[u8; 16]>,
    pub component_depth: Option<u32>,
    pub horizontal_subsampling: Option<u32>,
    pub vertical_subsampling: Option<u32>,
    pub audio_sampling_rate: Option<(i32, i32)>,
    pub locked: Option<bool>,
    pub channel_count: Option<u32>,
    pub quantization_bits: Option<u32>,
    pub sound_essence_coding: Option<[u8; 16]>,
    pub block_align: Option<u16>,
    pub average_bytes_per_second: Option<u32>,
}

/// A Timeline Track of a package with its descriptor and the essence container it lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MxfTrack {
    pub package_kind: MxfPackageKind,
    /// The package's UMID (32 bytes) when present.
    pub package_uid: Option<[u8; 32]>,
    pub track_id: u32,
    /// Essence element key bytes 13-16 (0 for tracks without essence, e.g. timecode).
    pub track_number: u32,
    pub kind: MxfTrackKind,
    pub data_definition: Option<[u8; 16]>,
    pub edit_rate: Option<(i32, i32)>,
    pub origin: Option<i64>,
    /// Duration of the track's sequence in edit units.
    pub duration: Option<i64>,
    /// First source clip of the track's sequence (material package tracks point at file package tracks).
    pub source_package: Option<[u8; 32]>,
    pub source_track_id: Option<u32>,
    pub body_sid: Option<u32>,
    pub index_sid: Option<u32>,
    pub descriptor: Option<MxfDescriptor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MxfDeltaEntry {
    /// -1: temporally reordered element; 0: not reordered; > 0: index into the position table.
    pub pos_table_index: i8,
    pub slice: u8,
    pub element_delta: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MxfIndexEntry {
    pub temporal_offset: i8,
    pub key_frame_offset: i8,
    /// Bit 7 random access, bit 6 sequence header, bits 5-4 prediction (00 I, 10 P, 01 / 11 B).
    pub flags: u8,
    pub stream_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MxfIndexSegment {
    pub index_sid: u32,
    pub body_sid: u32,
    pub edit_rate: (i32, i32),
    pub start_position: i64,
    pub duration: i64,
    /// Non-zero: every edit unit has this many bytes and `entries` may be empty.
    pub edit_unit_byte_count: u32,
    pub slice_count: u8,
    pub pos_table_count: u8,
    pub delta_entries: Vec<MxfDeltaEntry>,
    pub entries: Vec<MxfIndexEntry>,
    /// `slice_count` offsets per entry, flattened in entry order (slice 1.. relative to the stream offset).
    pub slice_offsets: Vec<u32>,
}

/// All segments sharing one IndexSID, in position order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MxfIndexTable {
    pub index_sid: u32,
    pub body_sid: u32,
    pub edit_rate: (i32, i32),
    pub delta_entries: Vec<MxfDeltaEntry>,
    pub slice_count: u8,
    pub segments: Vec<MxfIndexSegment>,
    bases: Vec<Option<u64>>,
}

/// How stream offsets map into the file (11.1.4 stream offset rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MxfWrapping {
    /// The offset is that of the first KLV key of the edit unit.
    Frame,
    /// The offset is relative to the first byte after the KL of the clip element in each partition.
    Clip,
}

/// Where one edit unit of stored essence is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MxfEditUnit {
    /// Requested position, in display order.
    pub position: u64,
    /// Position of the stored entry holding the unit (`position + temporal_offset`).
    pub coded_position: u64,
    pub stream_offset: u64,
    /// Absolute file offset: the first KLV key of the unit (frame wrapping) or its first byte (clip).
    pub file_offset: u64,
    pub temporal_offset: i8,
    pub key_frame_offset: i8,
    pub flags: u8,
    pub wrapping: MxfWrapping,
    /// Constant-size indexes: the byte count of every edit unit.
    pub edit_unit_byte_count: Option<u32>,
    pub index_sid: u32,
    pub body_sid: u32,
}

impl MxfEditUnit {
    /// Flags bit 7: decoding can start here. Constant-size indexes (intra essence) report true.
    pub fn random_access(&self) -> bool {
        self.flags & 0x80 != 0
    }
}

/// An essence element inside an edit unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MxfElement {
    /// File offset of the element's key (frame wrapping) or of its first byte (clip).
    pub key_offset: u64,
    pub value_offset: u64,
    pub value_len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MxfIndex {
    pub source_len: u64,
    /// Bytes before the header partition pack (6.6).
    pub run_in: u64,
    pub partitions: Vec<MxfPartition>,
    pub operational_pattern: [u8; 16],
    pub essence_container_labels: Vec<[u8; 16]>,
    pub tracks: Vec<MxfTrack>,
    pub index_tables: Vec<MxfIndexTable>,
    /// The partition the metadata came from carries a complete (final) Header Metadata.
    pub header_metadata_complete: bool,
    /// The partition chain ended in a cut (recording stopped inside a KLV).
    pub truncated_tail: bool,
    pub bytes_read: u64,
}

struct Counting<'a> {
    inner: &'a mut dyn ReadSeek,
    bytes: u64,
}

impl Read for Counting<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes = self.bytes.saturating_add(n as u64);
        Ok(n)
    }
}

impl Seek for Counting<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Skip KLV Fill items from `pos` (they pad to the KAG after a pack, header metadata or index segment).
fn skip_fills(src: &mut dyn ReadSeek, mut pos: u64, end: u64) -> u64 {
    for _ in 0..MAX_FILLS {
        match read_klv(src, pos, end) {
            Ok(klv) if is_fill(&klv.key) => pos = klv.end(),
            _ => break,
        }
    }
    pos
}

/// Fill in the structure offsets of `partitions[i]` (7.1 order: pack, fill, header metadata, index
/// segments, essence) once the next partition's start is known.
fn layout_partition(
    src: &mut dyn ReadSeek,
    p: &mut MxfPartition,
    end: u64,
) -> Result<(), ReelError> {
    p.end = end;
    let mut pos = skip_fills(src, p.pack_end, end);
    p.header_start = pos;
    pos = pos
        .checked_add(p.header_byte_count)
        .filter(|at| *at <= end)
        .ok_or_else(|| corrupt("mxf-header-byte-count"))?;
    p.index_start = pos;
    pos = pos
        .checked_add(p.index_byte_count)
        .filter(|at| *at <= end)
        .ok_or_else(|| corrupt("mxf-index-byte-count"))?;
    if p.body_sid != 0 {
        pos = skip_fills(src, pos, end);
        if pos < end {
            p.essence_start = Some(pos);
            p.essence_first_klv = read_klv(src, pos, end).ok().map(|k| MxfKlvInfo {
                key: k.key,
                header_len: k.header_len,
                value_len: k.value_len,
            });
        }
    }
    Ok(())
}

/// Footer chain: footer pack -> `previous_partition` links -> header pack. Every link must land on a pack
/// whose own `this_partition` agrees; any inconsistency returns `None` (the caller scans instead).
fn partition_chain(
    src: &mut dyn ReadSeek,
    header: &MxfPartition,
    run_in: u64,
    file_len: u64,
    cancel: &CancellationToken,
) -> Option<Vec<MxfPartition>> {
    let mut pos = absolute(run_in, header.footer_partition, file_len)?;
    let mut chain = Vec::new();
    loop {
        check_cancel(cancel).ok()?;
        if chain.len() >= MAX_PARTITIONS {
            return None;
        }
        let p = read_partition(src, pos, file_len).ok()?;
        if p.this_partition != pos.checked_sub(run_in)? {
            return None;
        }
        let previous = p.previous_partition;
        chain.push(p);
        if pos == run_in {
            break;
        }
        let next = run_in.checked_add(previous)?;
        if next >= pos {
            return None;
        }
        pos = next;
    }
    chain.reverse();
    (chain.first()?.kind == MxfPartitionKind::Header
        && chain.last()?.kind == MxfPartitionKind::Footer)
        .then_some(chain)
}

/// Walk the KLV stream from the header pack; jumps over header metadata and index regions by their byte
/// counts, over essence by KLV lengths. Used when the footer chain is absent or inconsistent (files whose
/// header says `footer_partition = 0`, recordings that were cut).
fn partition_scan(
    src: &mut dyn ReadSeek,
    header: MxfPartition,
    file_len: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<(Vec<MxfPartition>, bool), ReelError> {
    let mut out = Vec::new();
    let mut pos = header.pack_end;
    let mut skip = header
        .header_byte_count
        .saturating_add(header.index_byte_count);
    out.push(header);
    let max_klv = limits.max_samples.max(1024);
    for _ in 0..max_klv {
        check_cancel(cancel)?;
        pos = skip_fills(src, pos, file_len);
        pos = pos.saturating_add(skip);
        skip = 0;
        if pos >= file_len {
            return Ok((out, false));
        }
        let klv = match read_klv(src, pos, file_len) {
            Ok(klv) => klv,
            Err(_) => return Ok((out, true)),
        };
        if partition_pack(&klv.key).is_some() {
            if out.len() >= MAX_PARTITIONS {
                return Err(ReelError::LimitExceeded {
                    what: "mxf-partitions",
                });
            }
            let p = read_partition(src, pos, file_len)?;
            pos = p.pack_end;
            skip = p.header_byte_count.saturating_add(p.index_byte_count);
            out.push(p);
        } else {
            pos = klv.end();
        }
    }
    Err(ReelError::LimitExceeded {
        what: "mxf-klv-count",
    })
}

/// Read the structure of an MXF file: partitions, Header Metadata, index tables. Reads no essence payload.
pub fn build_mxf_index(
    src: &mut dyn ReadSeek,
    file_len: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<MxfIndex, ReelError> {
    let mut counting = Counting {
        inner: src,
        bytes: 0,
    };
    let built = build_inner(&mut counting, file_len, limits, cancel);
    let bytes = counting.bytes;
    built.map(|mut index| {
        index.bytes_read = bytes;
        index
    })
}

fn build_inner(
    src: &mut dyn ReadSeek,
    file_len: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<MxfIndex, ReelError> {
    let run_in = find_header_partition(src, file_len)?;
    let header = read_partition(src, run_in, file_len)?;
    let operational_pattern = header.operational_pattern;
    let essence_container_labels = header.essence_containers.clone();
    let (mut partitions, truncated_tail) =
        match partition_chain(src, &header, run_in, file_len, cancel) {
            Some(chain) => (chain, false),
            None => partition_scan(src, header, file_len, limits, cancel)?,
        };
    for i in 0..partitions.len() {
        check_cancel(cancel)?;
        let end = partitions
            .get(i.saturating_add(1))
            .map_or(file_len, |next| next.offset);
        if let Some(p) = partitions.get_mut(i) {
            layout_partition(src, p, end)?;
        }
    }

    // Header Metadata: the last COMPLETE copy (header, body repetition or footer), else the first one.
    let carrier = partitions
        .iter()
        .rev()
        .find(|p| p.header_byte_count > 0 && matches!(p.status, 3 | 4))
        .or_else(|| partitions.iter().find(|p| p.header_byte_count > 0));
    let mut probe_budget = limits.max_probe_bytes;
    let (tracks, op_from_metadata, header_metadata_complete) = match carrier {
        Some(p) => {
            if p.header_byte_count > probe_budget {
                return Err(ReelError::LimitExceeded {
                    what: "probe-bytes",
                });
            }
            probe_budget = probe_budget.saturating_sub(p.header_byte_count);
            let region = read_at(
                src,
                p.header_start,
                usize::try_from(p.header_byte_count).map_err(|_| ReelError::LimitExceeded {
                    what: "probe-bytes",
                })?,
                "mxf-header-metadata",
            )?;
            let resolved = meta::resolve(&region, limits)?;
            (
                resolved.tracks,
                resolved.operational_pattern,
                matches!(p.status, 3 | 4),
            )
        }
        None => (Vec::new(), None, false),
    };

    // Index Table Segments of every partition (header / body / footer copies collapse later).
    let mut entries_budget = limits.max_samples;
    let mut segments = Vec::new();
    for p in &partitions {
        check_cancel(cancel)?;
        if p.index_byte_count == 0 {
            continue;
        }
        if p.index_byte_count > probe_budget {
            return Err(ReelError::LimitExceeded {
                what: "probe-bytes",
            });
        }
        probe_budget = probe_budget.saturating_sub(p.index_byte_count);
        let region = read_at(
            src,
            p.index_start,
            usize::try_from(p.index_byte_count).map_err(|_| ReelError::LimitExceeded {
                what: "probe-bytes",
            })?,
            "mxf-index-region",
        )?;
        tables::parse_region(
            &region,
            &mut entries_budget,
            &mut segments,
            p.index_start,
            limits,
        )?;
    }
    let index_tables = tables::build_tables(segments)?;
    Ok(MxfIndex {
        source_len: file_len,
        run_in,
        partitions,
        operational_pattern: op_from_metadata.unwrap_or(operational_pattern),
        essence_container_labels,
        tracks,
        index_tables,
        header_metadata_complete,
        truncated_tail,
        bytes_read: 0,
    })
}

impl MxfIndex {
    pub fn index_table(&self, index_sid: u32) -> Option<&MxfIndexTable> {
        self.index_tables.iter().find(|t| t.index_sid == index_sid)
    }

    /// Essence track by TrackID in a Source (file) package.
    pub fn essence_track(&self, track_id: u32) -> Option<&MxfTrack> {
        self.tracks.iter().find(|t| {
            t.package_kind == MxfPackageKind::Source
                && t.track_id == track_id
                && t.track_number != 0
        })
    }

    /// First picture track with an index, else the first sound track with one.
    pub fn default_track(&self) -> Option<&MxfTrack> {
        let indexed = |kind: MxfTrackKind| {
            self.tracks.iter().find(|t| {
                t.package_kind == MxfPackageKind::Source
                    && t.kind == kind
                    && t.track_number != 0
                    && t.index_sid.is_some()
            })
        };
        indexed(MxfTrackKind::Picture).or_else(|| indexed(MxfTrackKind::Sound))
    }

    /// Frame or clip wrapping of the essence container indexed by `table` (see `MxfWrapping`). OP-Atom
    /// with a constant-size index is clip wrapped; a Generic Container label whose wrapping byte is 2 is
    /// clip wrapped; everything else is frame wrapped. A wrong assumption is caught when the offset is
    /// used (`edit_unit_element` checks the KLV it lands on).
    pub fn wrapping_of(&self, table: &MxfIndexTable) -> MxfWrapping {
        let atom = self.operational_pattern[12] == 0x10;
        let constant = table.segments.iter().any(|s| s.edit_unit_byte_count != 0);
        let labelled_clip = self
            .tracks
            .iter()
            .filter(|t| t.body_sid == Some(table.body_sid))
            .filter_map(|t| t.descriptor.as_ref()?.essence_container)
            .any(|label| label[8..13] == [0x0D, 0x01, 0x03, 0x01, 0x02] && label[14] == 0x02);
        if (atom && constant) || labelled_clip {
            MxfWrapping::Clip
        } else {
            MxfWrapping::Frame
        }
    }

    /// Absolute file offset of `stream_offset` in the essence container `body_sid`: the partition holding
    /// it is the last one with `body_offset <= stream_offset` (7.1 BodyOffset); clip wrapped essence skips
    /// the KL at the start of that partition's segment.
    fn file_offset(
        &self,
        body_sid: u32,
        stream_offset: u64,
        wrapping: MxfWrapping,
    ) -> Result<u64, ReelError> {
        let holders: Vec<&MxfPartition> = self
            .partitions
            .iter()
            .filter(|p| p.body_sid == body_sid && p.essence_start.is_some())
            .collect();
        let at = holders
            .partition_point(|p| p.body_offset <= stream_offset)
            .checked_sub(1)
            .ok_or_else(|| corrupt("mxf-stream-offset-outside-essence"))?;
        let p = holders[at];
        let start = p
            .essence_start
            .ok_or_else(|| corrupt("mxf-stream-offset-outside-essence"))?;
        let skip = match wrapping {
            MxfWrapping::Frame => 0,
            MxfWrapping::Clip => p.essence_first_klv.map_or(0, |k| k.header_len),
        };
        // A clip wrapped offset must stay inside the clip element (its value ends where the stream does).
        let limit = match (wrapping, p.essence_first_klv) {
            (MxfWrapping::Clip, Some(klv)) => start
                .checked_add(klv.header_len)
                .and_then(|first| first.checked_add(klv.value_len))
                .map_or(p.end, |clip_end| clip_end.min(p.end)),
            _ => p.end,
        };
        stream_offset
            .checked_sub(p.body_offset)
            .and_then(|delta| start.checked_add(skip)?.checked_add(delta))
            .filter(|file| *file < limit)
            .ok_or_else(|| corrupt("mxf-stream-offset-outside-essence"))
    }

    /// Byte offset of edit unit `position` (display order) of the index table `index_sid`.
    pub fn locate(&self, index_sid: u32, position: u64) -> Result<MxfEditUnit, ReelError> {
        let table = self.index_table(index_sid).ok_or(ReelError::Unsupported {
            what: "mxf-index-table-not-found",
        })?;
        let display_pos = i64::try_from(position).map_err(|_| corrupt("mxf-index-position"))?;
        let display = table.entry_at(display_pos)?;
        let coded_pos = display_pos
            .checked_add(i64::from(display.temporal_offset))
            .filter(|c| *c >= 0)
            .ok_or_else(|| corrupt("mxf-temporal-offset"))?;
        let coded = if display.temporal_offset == 0 {
            table.entry_at(display_pos)?
        } else {
            table.entry_at(coded_pos)?
        };
        let wrapping = self.wrapping_of(table);
        let eubc = table
            .segments
            .iter()
            .find(|s| s.start_position <= display_pos && s.edit_unit_byte_count != 0)
            .map(|s| s.edit_unit_byte_count);
        Ok(MxfEditUnit {
            position,
            coded_position: u64::try_from(coded_pos).map_err(|_| corrupt("mxf-temporal-offset"))?,
            stream_offset: coded.stream_offset,
            file_offset: self.file_offset(table.body_sid, coded.stream_offset, wrapping)?,
            temporal_offset: display.temporal_offset,
            key_frame_offset: coded.key_frame_offset,
            flags: coded.flags,
            wrapping,
            edit_unit_byte_count: eubc,
            index_sid,
            body_sid: table.body_sid,
        })
    }

    /// Unit stored at CODED position `coded_position` (no temporal reordering applied): the entry the
    /// stream offset belongs to. Decode order delivery walks these.
    pub fn locate_coded(
        &self,
        index_sid: u32,
        coded_position: u64,
    ) -> Result<MxfEditUnit, ReelError> {
        let table = self.index_table(index_sid).ok_or(ReelError::Unsupported {
            what: "mxf-index-table-not-found",
        })?;
        let pos = i64::try_from(coded_position).map_err(|_| corrupt("mxf-index-position"))?;
        let coded = table.entry_at(pos)?;
        let wrapping = self.wrapping_of(table);
        Ok(MxfEditUnit {
            position: coded_position,
            coded_position,
            stream_offset: coded.stream_offset,
            file_offset: self.file_offset(table.body_sid, coded.stream_offset, wrapping)?,
            temporal_offset: coded.temporal_offset,
            key_frame_offset: coded.key_frame_offset,
            flags: coded.flags,
            wrapping,
            edit_unit_byte_count: table
                .segments
                .iter()
                .find(|s| s.start_position <= pos && s.edit_unit_byte_count != 0)
                .map(|s| s.edit_unit_byte_count),
            index_sid,
            body_sid: table.body_sid,
        })
    }

    /// Display position of the unit stored at coded position `coded` (the inverse of `locate`'s
    /// reordering): the display entry `d` with `d + temporal_offset(d) == coded`. Temporal offsets are
    /// signed bytes, so only positions within 128 of `coded` can match.
    pub fn display_position_of(&self, index_sid: u32, coded: u64) -> Result<u64, ReelError> {
        let table = self.index_table(index_sid).ok_or(ReelError::Unsupported {
            what: "mxf-index-table-not-found",
        })?;
        let lo = coded.saturating_sub(128);
        let hi = coded.saturating_add(128);
        for d in lo..=hi {
            let Ok(pos) = i64::try_from(d) else { break };
            if let Ok(entry) = table.entry_at(pos) {
                let target = pos.checked_add(i64::from(entry.temporal_offset));
                if target == i64::try_from(coded).ok() {
                    return Ok(d);
                }
            }
        }
        Err(corrupt("mxf-temporal-offset"))
    }

    /// Spec 11.4: file offset of delta entry `delta_index` of edit unit `position`
    /// (`entry stream offset + slice offset + element delta`). Elements marked temporally reordered
    /// (`pos_table_index < 0`) use the coded entry, the others the entry at `position` itself.
    pub fn element_offset(
        &self,
        index_sid: u32,
        position: u64,
        delta_index: usize,
    ) -> Result<u64, ReelError> {
        let table = self.index_table(index_sid).ok_or(ReelError::Unsupported {
            what: "mxf-index-table-not-found",
        })?;
        let delta = table
            .delta_entries
            .get(delta_index)
            .ok_or_else(|| corrupt("mxf-delta-entry"))?;
        let display_pos = i64::try_from(position).map_err(|_| corrupt("mxf-index-position"))?;
        let own = table.entry_at(display_pos)?;
        let entry = if delta.pos_table_index < 0 && own.temporal_offset != 0 {
            let coded_pos = display_pos
                .checked_add(i64::from(own.temporal_offset))
                .filter(|c| *c >= 0)
                .ok_or_else(|| corrupt("mxf-temporal-offset"))?;
            table.entry_at(coded_pos)?
        } else {
            own
        };
        let slice_start = match delta.slice {
            0 => 0u64,
            s => u64::from(
                *entry
                    .slices
                    .get(usize::from(s).saturating_sub(1))
                    .ok_or_else(|| corrupt("mxf-slice-offset"))?,
            ),
        };
        let stream = entry
            .stream_offset
            .checked_add(slice_start)
            .and_then(|o| o.checked_add(u64::from(delta.element_delta)))
            .ok_or_else(|| corrupt("mxf-index-offset"))?;
        self.file_offset(table.body_sid, stream, self.wrapping_of(table))
    }

    /// End (exclusive) of the clip element that holds clip wrapped `unit`.
    pub fn clip_end(&self, unit: &MxfEditUnit) -> Result<u64, ReelError> {
        self.partitions
            .iter()
            .filter(|p| p.body_sid == unit.body_sid)
            .filter_map(|p| p.essence_first_klv.zip(p.essence_start))
            .filter_map(|(klv, start)| {
                let first = start.checked_add(klv.header_len)?;
                let end = first.checked_add(klv.value_len)?;
                (first <= unit.file_offset && unit.file_offset < end).then_some(end)
            })
            .next()
            .ok_or_else(|| corrupt("mxf-index-points-outside-essence"))
    }

    /// The essence element with `track_number` in `unit`, found by inspecting the keys of the unit's
    /// KLVs (11.1.4: which elements an index entry covers is determined by their keys). Frame wrapping:
    /// the KLVs from the unit start up to the next unit; clip wrapping: the unit's byte range inside the
    /// clip element. The KLV the offset lands on must be an essence element or fill, otherwise the index
    /// and the file disagree (`Corrupt`).
    pub fn edit_unit_element(
        &self,
        src: &mut dyn ReadSeek,
        unit: &MxfEditUnit,
        track_number: u32,
        limits: &Limits,
    ) -> Result<MxfElement, ReelError> {
        let partition_end = self
            .partitions
            .iter()
            .filter(|p| p.body_sid == unit.body_sid)
            .filter(|p| p.essence_start.is_some_and(|s| s <= unit.file_offset))
            .map(|p| p.end)
            .find(|end| unit.file_offset < *end)
            .ok_or_else(|| corrupt("mxf-stream-offset-outside-essence"))?;
        if unit.wrapping == MxfWrapping::Clip {
            let bytes = u64::from(unit.edit_unit_byte_count.ok_or(ReelError::Unsupported {
                what: "mxf-clip-wrapped-variable-size",
            })?);
            let clip_end = self.clip_end(unit)?;
            let len = clip_end.saturating_sub(unit.file_offset).min(bytes);
            return Ok(MxfElement {
                key_offset: unit.file_offset,
                value_offset: unit.file_offset,
                value_len: len,
            });
        }
        let mut pos = unit.file_offset;
        let mut seen_essence = false;
        for _ in 0..MAX_ELEMENTS_PER_UNIT {
            let klv = read_klv(src, pos, partition_end)?;
            if is_fill(&klv.key) {
                pos = klv.end();
                continue;
            }
            let Some(item) = generic_container_item(&klv.key) else {
                // Not part of a content package: the unit ended (partition boundary) or the index does
                // not point at essence at all.
                if pos == unit.file_offset {
                    return Err(corrupt("mxf-index-points-outside-essence"));
                }
                break;
            };
            // A system item after essence starts the next content package.
            if item == 0x04 && seen_essence {
                break;
            }
            seen_essence |= item != 0x04;
            if essence_element(&klv.key) == Some(track_number) {
                if klv.value_len > limits.max_chunk_bytes as u64 {
                    return Err(ReelError::LimitExceeded {
                        what: "sample-bytes",
                    });
                }
                return Ok(MxfElement {
                    key_offset: pos,
                    value_offset: klv.value_pos(),
                    value_len: klv.value_len,
                });
            }
            pos = klv.end();
        }
        Err(corrupt("mxf-element-not-found"))
    }
}

/// Ticks of `units` edit units at `rate` (numerator / denominator per second): exact when the division
/// is whole, else rounded to nearest (ties up) with `exact == false`.
pub(crate) fn units_to_ticks(units: u64, rate: (i32, i32)) -> Result<(u64, bool), ReelError> {
    let (num, den) = (
        u128::from(rate.0.max(1) as u32),
        u128::from(rate.1.max(1) as u32),
    );
    let scaled = u128::from(units)
        .checked_mul(den)
        .and_then(|v| v.checked_mul(u128::from(hsk_studio_accord::TICKS_PER_SECOND)))
        .ok_or(ReelError::LimitExceeded { what: "tick-range" })?;
    let whole = scaled.checked_div(num).unwrap_or(0);
    let rem = scaled.checked_rem(num).unwrap_or(0);
    let rounded = if rem.saturating_mul(2) >= num {
        whole.saturating_add(1)
    } else {
        whole
    };
    let ticks = u64::try_from(if rem == 0 { whole } else { rounded })
        .map_err(|_| ReelError::LimitExceeded { what: "tick-range" })?;
    Ok((ticks, rem == 0))
}

/// Edit unit containing `ticks` and whether `ticks` is exactly its start.
pub(crate) fn ticks_to_unit(ticks: u64, rate: (i32, i32)) -> Result<(u64, bool), ReelError> {
    let (num, den) = (
        u128::from(rate.0.max(1) as u32),
        u128::from(rate.1.max(1) as u32),
    );
    let denominator = den
        .checked_mul(u128::from(hsk_studio_accord::TICKS_PER_SECOND))
        .ok_or(ReelError::LimitExceeded { what: "tick-range" })?;
    let scaled = u128::from(ticks)
        .checked_mul(num)
        .ok_or(ReelError::LimitExceeded { what: "tick-range" })?;
    let unit = u64::try_from(scaled.checked_div(denominator).unwrap_or(0))
        .map_err(|_| ReelError::LimitExceeded { what: "tick-range" })?;
    Ok((unit, scaled.checked_rem(denominator) == Some(0)))
}
