//! Index Table Segments (ST 377-1 section 11.2.3) and edit unit lookup (11.4, 11.6.2).
//!
//! A segment is a Local Set with STATIC tags (the Primer is not used for index tables, 9.2): Index Edit
//! Rate 3F.0B, Index Start Position 3F.0C, Index Duration 3F.0D, Edit Unit Byte Count 3F.05, IndexSID
//! 3F.06, BodySID 3F.07, Slice Count 3F.08, Pos Table Count 3F.0E, Delta Entry Array 3F.09, Index Entry
//! Array 3F.0A. Delta entries are `PosTableIndex i8, Slice u8, ElementDelta u32`; index entries are
//! `TemporalOffset i8, KeyFrameOffset i8, Flags u8, StreamOffset u64, SliceOffset u32 x NSL,
//! PosTable (i32,i32) x NPE`.
//!
//! Constant edit unit size (EditUnitByteCount != 0, 11.1.9): the byte offset of edit unit N is
//! `N x EditUnitByteCount` (the index entry array may be omitted); when the first edit unit differs the
//! file carries a second segment starting at 1 and offsets continue from the first segment's size.
//! Variable size: the entry for edit unit N holds its stream offset. Temporal reordering (11.1.6): the
//! TemporalOffset stored with display position N gives the CODED position `N + offset`; the stream
//! offset, flags and key frame offset of that coded entry describe the picture shown at N.
//!
//! Hostile input: the number of segments, delta entries and index entries is bounded by `Limits` (the
//! aggregate over all segments, checked before allocation), entry lengths must match the declared slice
//! and position table counts, repeated segments (header and footer copies) must be identical.
use super::klv::{corrupt, is_fill, is_index_segment, parse_klv_head};
use super::{MxfDeltaEntry, MxfIndexEntry, MxfIndexSegment, MxfIndexTable};
use crate::error::ReelError;
use crate::model::Limits;

const MAX_DELTA_ENTRIES: usize = 4096;

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(
        b.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// Parse every Index Table Segment in an index region (segments and Fill items). `budget` is the number
/// of index entries still allowed across the whole file.
pub(super) fn parse_region(
    region: &[u8],
    entries_budget: &mut usize,
    out: &mut Vec<(u64, MxfIndexSegment)>,
    region_offset: u64,
    limits: &Limits,
) -> Result<(), ReelError> {
    let mut at = 0usize;
    while region.len().saturating_sub(at) > 16 {
        let klv = parse_klv_head(&region[at..], region_offset.saturating_add(at as u64))?;
        let value_at = usize::try_from(klv.header_len)
            .ok()
            .and_then(|h| at.checked_add(h))
            .ok_or_else(|| corrupt("mxf-index-klv"))?;
        let end = usize::try_from(klv.value_len)
            .ok()
            .and_then(|l| value_at.checked_add(l))
            .filter(|end| *end <= region.len())
            .ok_or_else(|| corrupt("klv-length-exceeds-source"))?;
        let value = &region[value_at..end];
        let segment_offset = klv.pos;
        at = end;
        if is_fill(&klv.key) {
            continue;
        }
        if !is_index_segment(&klv.key) {
            continue; // not an index segment: skipped by its length
        }
        if out.len() >= limits.max_samples.min(1 << 20) {
            return Err(ReelError::LimitExceeded {
                what: "mxf-index-segments",
            });
        }
        let segment = parse_segment(value, klv.key[5] == 0x13, entries_budget)?;
        out.push((segment_offset, segment));
    }
    Ok(())
}

fn parse_segment(
    value: &[u8],
    ber_lengths: bool,
    entries_budget: &mut usize,
) -> Result<MxfIndexSegment, ReelError> {
    let mut seg = MxfIndexSegment {
        index_sid: 0,
        body_sid: 0,
        edit_rate: (0, 0),
        start_position: 0,
        duration: 0,
        edit_unit_byte_count: 0,
        slice_count: 0,
        pos_table_count: 0,
        delta_entries: Vec::new(),
        entries: Vec::new(),
        slice_offsets: Vec::new(),
    };
    let (mut at, mut have_rate) = (0usize, false);
    let (mut delta_raw, mut entry_raw): (Option<&[u8]>, Option<&[u8]>) = (None, None);
    while at < value.len() {
        let tag = value
            .get(at..at.saturating_add(2))
            .and_then(|b| <[u8; 2]>::try_from(b).ok())
            .map(u16::from_be_bytes)
            .ok_or_else(|| corrupt("mxf-index-tag"))?;
        at = at.saturating_add(2);
        let len = if ber_lengths {
            let head = value
                .get(at..)
                .filter(|r| !r.is_empty())
                .ok_or_else(|| corrupt("mxf-index-length"))?;
            let first = head[0];
            if first < 0x80 {
                at = at.saturating_add(1);
                usize::from(first)
            } else {
                let n = usize::from(first & 0x7F);
                let octets = head
                    .get(1..1usize.saturating_add(n))
                    .filter(|_| (1..=8).contains(&n))
                    .ok_or_else(|| corrupt("mxf-index-length"))?;
                at = at.saturating_add(1).saturating_add(n);
                usize::try_from(octets.iter().fold(0u64, |a, b| (a << 8) | u64::from(*b)))
                    .map_err(|_| corrupt("mxf-index-length"))?
            }
        } else {
            let l = value
                .get(at..at.saturating_add(2))
                .and_then(|b| <[u8; 2]>::try_from(b).ok())
                .map(u16::from_be_bytes)
                .ok_or_else(|| corrupt("mxf-index-length"))?;
            at = at.saturating_add(2);
            usize::from(l)
        };
        let body = value
            .get(at..at.saturating_add(len))
            .ok_or_else(|| corrupt("mxf-index-length"))?;
        at = at.saturating_add(len);
        match tag {
            0x3F0B => {
                let (n, d) = (
                    be32(body, 0).ok_or_else(|| corrupt("mxf-index-edit-rate"))?,
                    be32(body, 4).ok_or_else(|| corrupt("mxf-index-edit-rate"))?,
                );
                seg.edit_rate = (n as i32, d as i32);
                have_rate = true;
            }
            0x3F0C => {
                seg.start_position =
                    be64(body, 0).ok_or_else(|| corrupt("mxf-index-start"))? as i64;
            }
            0x3F0D => {
                seg.duration = be64(body, 0).ok_or_else(|| corrupt("mxf-index-duration"))? as i64;
            }
            0x3F05 => {
                seg.edit_unit_byte_count =
                    be32(body, 0).ok_or_else(|| corrupt("mxf-index-eubc"))?;
            }
            0x3F06 => seg.index_sid = be32(body, 0).ok_or_else(|| corrupt("mxf-index-sid"))?,
            0x3F07 => seg.body_sid = be32(body, 0).ok_or_else(|| corrupt("mxf-index-sid"))?,
            0x3F08 => seg.slice_count = *body.first().ok_or_else(|| corrupt("mxf-index-slices"))?,
            0x3F0E => {
                seg.pos_table_count =
                    *body.first().ok_or_else(|| corrupt("mxf-index-pos-table"))?;
            }
            0x3F09 => delta_raw = Some(body),
            0x3F0A => entry_raw = Some(body),
            _ => {}
        }
    }
    if !have_rate || seg.edit_rate.0 <= 0 || seg.edit_rate.1 <= 0 {
        return Err(corrupt("mxf-index-edit-rate"));
    }
    if seg.start_position < 0 || seg.duration < 0 {
        return Err(corrupt("mxf-index-range"));
    }
    if let Some(raw) = delta_raw {
        let n = be32(raw, 0).ok_or_else(|| corrupt("mxf-delta-array"))? as usize;
        let item = be32(raw, 4).ok_or_else(|| corrupt("mxf-delta-array"))? as usize;
        let body = raw.get(8..).unwrap_or_default();
        if item != 6
            || n > MAX_DELTA_ENTRIES
            || n.checked_mul(6).is_none_or(|bytes| bytes != body.len())
        {
            return Err(corrupt("mxf-delta-array"));
        }
        seg.delta_entries = body
            .chunks_exact(6)
            .map(|e| MxfDeltaEntry {
                pos_table_index: e[0] as i8,
                slice: e[1],
                element_delta: u32::from_be_bytes([e[2], e[3], e[4], e[5]]),
            })
            .collect();
    }
    if let Some(raw) = entry_raw {
        let n = be32(raw, 0).ok_or_else(|| corrupt("mxf-entry-array"))? as usize;
        let item = be32(raw, 4).ok_or_else(|| corrupt("mxf-entry-array"))? as usize;
        let body = raw.get(8..).unwrap_or_default();
        // Entry length depends on the slice and position table counts (Table 28).
        let slices = usize::from(seg.slice_count);
        let positions = usize::from(seg.pos_table_count);
        let expected = 11usize
            .checked_add(
                slices
                    .checked_mul(4)
                    .ok_or_else(|| corrupt("mxf-entry-array"))?,
            )
            .and_then(|e| e.checked_add(positions.checked_mul(8)?))
            .ok_or_else(|| corrupt("mxf-entry-array"))?;
        if item != expected || n.checked_mul(item).is_none_or(|bytes| bytes != body.len()) {
            return Err(corrupt("mxf-entry-array"));
        }
        if n > *entries_budget {
            return Err(ReelError::LimitExceeded {
                what: "mxf-index-entries",
            });
        }
        *entries_budget = entries_budget.saturating_sub(n);
        seg.entries.reserve_exact(n);
        seg.slice_offsets.reserve_exact(
            n.checked_mul(slices)
                .ok_or_else(|| corrupt("mxf-entry-array"))?,
        );
        for e in body.chunks_exact(item) {
            seg.entries.push(MxfIndexEntry {
                temporal_offset: e[0] as i8,
                key_frame_offset: e[1] as i8,
                flags: e[2],
                stream_offset: be64(e, 3).ok_or_else(|| corrupt("mxf-entry-array"))?,
            });
            for s in 0..slices {
                let at = 11usize.saturating_add(s.saturating_mul(4));
                seg.slice_offsets
                    .push(be32(e, at).ok_or_else(|| corrupt("mxf-entry-array"))?);
            }
        }
    }
    // Variable size: the entry array defines the segment; its duration must agree.
    if seg.edit_unit_byte_count == 0 {
        let count = seg.entries.len() as i64;
        if seg.duration != count || count == 0 {
            return Err(corrupt("mxf-index-duration"));
        }
    }
    Ok(seg)
}

/// Group segments into tables by IndexSID. Identical repeats (header and footer copies, rule 11.2.1.4)
/// collapse into one; overlapping or inconsistent segments are corruption.
pub(super) fn build_tables(
    mut segments: Vec<(u64, MxfIndexSegment)>,
) -> Result<Vec<MxfIndexTable>, ReelError> {
    segments.sort_by_key(|(_, s)| (s.index_sid, s.start_position));
    let mut tables: Vec<MxfIndexTable> = Vec::new();
    for (_, seg) in segments {
        let continues = tables.last().is_some_and(|t| t.index_sid == seg.index_sid);
        if !continues {
            tables.push(MxfIndexTable {
                index_sid: seg.index_sid,
                body_sid: seg.body_sid,
                edit_rate: seg.edit_rate,
                delta_entries: seg.delta_entries.clone(),
                slice_count: seg.slice_count,
                segments: Vec::new(),
                bases: Vec::new(),
            });
        }
        let Some(table) = tables.last_mut() else {
            continue;
        };
        if table.body_sid != seg.body_sid
            || table.edit_rate != seg.edit_rate
            || table.slice_count != seg.slice_count
            || (!seg.delta_entries.is_empty() && table.delta_entries != seg.delta_entries)
        {
            return Err(corrupt("mxf-index-table-inconsistent"));
        }
        if table.delta_entries.is_empty() {
            table.delta_entries = seg.delta_entries.clone();
        }
        if let Some(prev) = table.segments.last() {
            if prev.start_position == seg.start_position {
                if prev.duration == seg.duration
                    && prev.edit_unit_byte_count == seg.edit_unit_byte_count
                    && prev.entries == seg.entries
                {
                    continue; // identical repeat
                }
                return Err(corrupt("mxf-index-overlap"));
            }
            if prev.edit_unit_byte_count == 0
                && prev
                    .start_position
                    .checked_add(prev.duration)
                    .is_none_or(|end| end > seg.start_position)
            {
                return Err(corrupt("mxf-index-overlap"));
            }
        }
        table.segments.push(seg);
    }
    for table in &mut tables {
        table.bases = constant_bases(&table.segments);
    }
    Ok(tables)
}

/// Stream offset of the first edit unit of every constant-size segment: known when every segment before
/// it is constant-size, starts where the previous one ends, and the first starts at 0 (11.1.9).
fn constant_bases(segments: &[MxfIndexSegment]) -> Vec<Option<u64>> {
    let mut bases = Vec::with_capacity(segments.len());
    let mut next: Option<(i64, u64)> = Some((0, 0)); // (next position, its stream offset)
    for seg in segments {
        if seg.edit_unit_byte_count == 0 {
            bases.push(None);
            next = None;
            continue;
        }
        let base = match next {
            Some((position, offset)) if position == seg.start_position => Some(offset),
            _ => None,
        };
        bases.push(base);
        next = match (base, seg.duration) {
            (Some(offset), duration) if duration > 0 => {
                seg.start_position.checked_add(duration).zip(
                    u64::try_from(duration)
                        .ok()
                        .and_then(|d| d.checked_mul(u64::from(seg.edit_unit_byte_count)))
                        .and_then(|bytes| offset.checked_add(bytes)),
                )
            }
            _ => None,
        };
    }
    bases
}

impl MxfIndexTable {
    fn segment_for(&self, position: i64) -> Option<(usize, &MxfIndexSegment)> {
        let at = self
            .segments
            .partition_point(|s| s.start_position <= position)
            .checked_sub(1)?;
        let seg = self.segments.get(at)?;
        let covered = if seg.edit_unit_byte_count != 0 && seg.duration == 0 {
            true // applies to every edit unit from its start (condition 1)
        } else {
            seg.start_position
                .checked_add(seg.duration)
                .is_some_and(|end| position < end)
        };
        covered.then_some((at, seg))
    }

    /// Stream offset, flags and key frame offset of the CODED entry at `position`, plus the entry's own
    /// temporal offset (VBE) or `None` for a constant-size segment.
    pub(super) fn entry_at(&self, position: i64) -> Result<CodedEntry<'_>, ReelError> {
        let (at, seg) = self.segment_for(position).ok_or(ReelError::OutOfRange {
            requested: hsk_studio_accord::Ticks::new(position.max(0) as u64),
            first: self.segments.first().map_or(0, |s| s.start_position),
            end: 0,
        })?;
        let rel = position
            .checked_sub(seg.start_position)
            .and_then(|r| usize::try_from(r).ok())
            .ok_or_else(|| corrupt("mxf-index-position"))?;
        if seg.edit_unit_byte_count != 0 {
            let base = self
                .bases
                .get(at)
                .copied()
                .flatten()
                .ok_or(ReelError::Unsupported {
                    what: "mxf-index-constant-segment-not-contiguous",
                })?;
            let offset = u64::try_from(rel)
                .ok()
                .and_then(|r| r.checked_mul(u64::from(seg.edit_unit_byte_count)))
                .and_then(|bytes| base.checked_add(bytes))
                .ok_or_else(|| corrupt("mxf-index-offset"))?;
            return Ok(CodedEntry {
                stream_offset: offset,
                flags: 0x80,
                key_frame_offset: 0,
                temporal_offset: 0,
                slices: &[],
            });
        }
        let entry = seg
            .entries
            .get(rel)
            .ok_or_else(|| corrupt("mxf-index-position"))?;
        let slices = usize::from(seg.slice_count);
        let slice_start = rel
            .checked_mul(slices)
            .ok_or_else(|| corrupt("mxf-index-position"))?;
        Ok(CodedEntry {
            stream_offset: entry.stream_offset,
            flags: entry.flags,
            key_frame_offset: entry.key_frame_offset,
            temporal_offset: entry.temporal_offset,
            slices: seg
                .slice_offsets
                .get(slice_start..slice_start.saturating_add(slices))
                .unwrap_or_default(),
        })
    }

    /// Number of edit units the table indexes (`None` when a constant-size segment has no end).
    pub fn indexed_units(&self) -> Option<u64> {
        let last = self.segments.last()?;
        if last.edit_unit_byte_count != 0 && last.duration == 0 {
            return None;
        }
        last.start_position
            .checked_add(last.duration)
            .and_then(|end| u64::try_from(end).ok())
    }
}

/// One looked-up index entry (borrowing the slice offsets of its segment).
pub(super) struct CodedEntry<'a> {
    pub stream_offset: u64,
    pub flags: u8,
    pub key_frame_offset: i8,
    pub temporal_offset: i8,
    pub slices: &'a [u32],
}
