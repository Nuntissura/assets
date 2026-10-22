//! KLV primitives, key classification and partition packs (SMPTE ST 377-1:2019 sections 6.3, 6.6, 7).
//!
//! KLV (ST 336): 16-byte key, BER length, value. BER: one byte below 0x80 is the length; 0x80 means
//! "unspecified length" and is not valid in MXF (6.3.4); 0x81..=0x88 announce that many big-endian length
//! octets (encoders use at most 9 BER bytes). A length that reaches past the source is a
//! typed `Corrupt`. Multi-byte values are big-endian (6.4.2).
use super::{MxfPartition, MxfPartitionKind};
use crate::error::ReelError;
use crate::model::{ReadSeek, read_exact_structural};
use std::io::SeekFrom;

pub(super) const KEY_LEN: usize = 16;
/// Longest key + BER length header (16 + 1 + 8).
const MAX_HEAD: u64 = 25;
/// The run-in must be shorter than this (6.6) and does not contain the partition label.
pub(super) const MAX_RUN_IN: u64 = 65_536;
/// A partition pack is a few hundred bytes (88 + 16 per essence container label).
const MAX_PACK_VALUE: u64 = 65_536;
const MAX_ESSENCE_CONTAINER_LABELS: usize = 1024;
/// Fixed part of a partition pack value: versions, KAG, six u64 offsets/counts, SIDs, operational
/// pattern, then the essence container batch header (count + item length).
const PACK_FIXED: usize = 88;

pub(super) fn corrupt(what: &'static str) -> ReelError {
    ReelError::Corrupt { what }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Klv {
    pub key: [u8; KEY_LEN],
    /// File offset of the first key byte.
    pub pos: u64,
    /// Key plus BER length.
    pub header_len: u64,
    pub value_len: u64,
}

impl Klv {
    pub(super) fn value_pos(&self) -> u64 {
        self.pos.saturating_add(self.header_len)
    }
    pub(super) fn end(&self) -> u64 {
        self.value_pos().saturating_add(self.value_len)
    }
}

/// Key and BER length from `buf`, which starts at the key; `pos` is where `buf` came from.
pub(super) fn parse_klv_head(buf: &[u8], pos: u64) -> Result<Klv, ReelError> {
    let key: [u8; KEY_LEN] = buf
        .get(..KEY_LEN)
        .and_then(|k| k.try_into().ok())
        .ok_or_else(|| corrupt("klv-truncated"))?;
    let first = *buf.get(KEY_LEN).ok_or_else(|| corrupt("klv-truncated"))?;
    let (value_len, ber_len) = if first < 0x80 {
        (u64::from(first), 1u64)
    } else if first == 0x80 {
        return Err(corrupt("klv-ber-unspecified"));
    } else {
        let count = usize::from(first & 0x7F);
        if count > 8 {
            return Err(corrupt("klv-ber-length"));
        }
        let start = KEY_LEN.saturating_add(1);
        let octets = buf
            .get(start..start.saturating_add(count))
            .ok_or_else(|| corrupt("klv-truncated"))?;
        let value = octets
            .iter()
            .fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
        (value, 1u64.saturating_add(count as u64))
    };
    Ok(Klv {
        key,
        pos,
        header_len: (KEY_LEN as u64).saturating_add(ber_len),
        value_len,
    })
}

pub(super) fn read_at(
    src: &mut dyn ReadSeek,
    pos: u64,
    len: usize,
    what: &'static str,
) -> Result<Vec<u8>, ReelError> {
    src.seek(SeekFrom::Start(pos))?;
    let mut buf = vec![0u8; len];
    read_exact_structural(src, &mut buf, what)?;
    Ok(buf)
}

/// The KLV starting at `pos`, which must lie wholly before `end` (`end` is the source or partition end).
pub(super) fn read_klv(src: &mut dyn ReadSeek, pos: u64, end: u64) -> Result<Klv, ReelError> {
    let room = end.saturating_sub(pos);
    if room < (KEY_LEN as u64).saturating_add(1) {
        return Err(corrupt("klv-truncated"));
    }
    let take = room.min(MAX_HEAD) as usize;
    let klv = parse_klv_head(&read_at(src, pos, take, "klv-header")?, pos)?;
    if klv.value_pos() > end || klv.value_len > end.saturating_sub(klv.value_pos()) {
        return Err(corrupt("klv-length-exceeds-source"));
    }
    Ok(klv)
}

// ---- key classification -------------------------------------------------------------------------------

const SMPTE: [u8; 4] = [0x06, 0x0E, 0x2B, 0x34];

fn is_smpte(key: &[u8; KEY_LEN]) -> bool {
    key[..4] == SMPTE
}

/// KLV Fill item (6.3.3): decoders ignore the version byte (byte 8) when matching it.
pub(super) fn is_fill(key: &[u8; KEY_LEN]) -> bool {
    is_smpte(key)
        && key[4..7] == [0x01, 0x01, 0x01]
        && key[8..] == [0x03, 0x01, 0x02, 0x10, 0x01, 0x00, 0x00, 0x00]
}

/// Partition pack key (Table 4): kind in byte 14, status in byte 15.
pub(super) fn partition_pack(key: &[u8; KEY_LEN]) -> Option<(MxfPartitionKind, u8)> {
    if !(is_smpte(key)
        && key[4..7] == [0x02, 0x05, 0x01]
        && key[8..13] == [0x0D, 0x01, 0x02, 0x01, 0x01]
        && (1..=4).contains(&key[14]))
    {
        return None;
    }
    let kind = match key[13] {
        0x02 => MxfPartitionKind::Header,
        0x03 => MxfPartitionKind::Body,
        0x04 => MxfPartitionKind::Footer,
        _ => return None,
    };
    Some((kind, key[14]))
}

pub(super) fn is_primer(key: &[u8; KEY_LEN]) -> bool {
    is_smpte(key)
        && key[4..7] == [0x02, 0x05, 0x01]
        && key[8..] == [0x0D, 0x01, 0x02, 0x01, 0x01, 0x05, 0x01, 0x00]
}

/// Index Table Segment (Table 25): a local set; byte 6 is 0x53 (2-byte lengths) or 0x13 (BER lengths).
pub(super) fn is_index_segment(key: &[u8; KEY_LEN]) -> bool {
    is_smpte(key)
        && key[4] == 0x02
        && matches!(key[5], 0x53 | 0x13)
        && key[6] == 0x01
        && key[8..] == [0x0D, 0x01, 0x02, 0x01, 0x01, 0x10, 0x01, 0x00]
}

/// Structural / descriptor set key (Table 16): the set kind is byte 15. `Some(true)` = BER local lengths.
pub(super) fn header_set(key: &[u8; KEY_LEN]) -> Option<(u8, bool)> {
    (is_smpte(key)
        && key[4] == 0x02
        && matches!(key[5], 0x53 | 0x13)
        && key[6] == 0x01
        && key[8..14] == [0x0D, 0x01, 0x01, 0x01, 0x01, 0x01])
    .then_some((key[14], key[5] == 0x13))
}

/// Generic Container essence element key: bytes 13-16 are the track number (item type, element count,
/// element type, element number).
pub(super) fn essence_element(key: &[u8; KEY_LEN]) -> Option<u32> {
    (is_smpte(key) && key[4..7] == [0x01, 0x02, 0x01] && key[8..12] == [0x0D, 0x01, 0x03, 0x01])
        .then(|| u32::from_be_bytes([key[12], key[13], key[14], key[15]]))
}

/// Any KLV of a Generic Container content package (ST 379): system items, picture / sound / data
/// elements. A content package is a run of these keys; the item type is key byte 13 (0x04 system item,
/// 0x15 picture, 0x16 sound, 0x17 data, 0x18 compound).
pub(super) fn generic_container_item(key: &[u8; KEY_LEN]) -> Option<u8> {
    (is_smpte(key) && key[8..12] == [0x0D, 0x01, 0x03, 0x01]).then_some(key[12])
}

/// First 11 bytes of the header partition pack label (6.6) at the start of `buf`: bytes 1-7, 9-11 and the
/// header kind; the version byte is ignored.
fn header_label_at(buf: &[u8]) -> bool {
    buf.len() >= KEY_LEN
        && buf[..4] == SMPTE
        && buf[4..7] == [0x02, 0x05, 0x01]
        && buf[8..13] == [0x0D, 0x01, 0x02, 0x01, 0x01]
        && buf[13] == 0x02
}

/// Offset of the header partition pack: the file starts with it, or a non-KLV run-in (< 65536 bytes)
/// precedes it (6.6).
pub(super) fn find_header_partition(
    src: &mut dyn ReadSeek,
    file_len: u64,
) -> Result<u64, ReelError> {
    let take = file_len.min(MAX_RUN_IN.saturating_add(KEY_LEN as u64)) as usize;
    let head = read_at(src, 0, take, "mxf-run-in")?;
    let not_mxf = || ReelError::ImportUnsupported {
        detected: "not-mxf".into(),
    };
    (0..take)
        .find(|at| head.get(*at..).is_some_and(header_label_at))
        .map(|at| at as u64)
        .filter(|at| *at < MAX_RUN_IN)
        .ok_or_else(not_mxf)
}

/// Parse a partition pack at `pos` (a KLV whose key is a partition pack key). Offsets stored in a pack
/// are relative to the header partition; the caller converts them.
pub(super) fn read_partition(
    src: &mut dyn ReadSeek,
    pos: u64,
    file_len: u64,
) -> Result<MxfPartition, ReelError> {
    let klv = read_klv(src, pos, file_len)?;
    let (kind, status) = partition_pack(&klv.key).ok_or_else(|| corrupt("mxf-partition-key"))?;
    if klv.value_len > MAX_PACK_VALUE || klv.value_len < PACK_FIXED as u64 {
        return Err(corrupt("mxf-partition-pack-length"));
    }
    let v = read_at(
        src,
        klv.value_pos(),
        klv.value_len as usize,
        "mxf-partition-pack",
    )?;
    let u16_at = |at: usize| u16::from_be_bytes([v[at], v[at.saturating_add(1)]]);
    let u32_at = |at: usize| {
        u32::from_be_bytes([
            v[at],
            v[at.saturating_add(1)],
            v[at.saturating_add(2)],
            v[at.saturating_add(3)],
        ])
    };
    let u64_at = |at: usize| {
        let mut b = [0u8; 8];
        b.copy_from_slice(&v[at..at.saturating_add(8)]);
        u64::from_be_bytes(b)
    };
    let labels = u32_at(80) as usize;
    let item_len = u32_at(84);
    let batch_bytes = v.len().saturating_sub(PACK_FIXED);
    if (labels > 0 && item_len != KEY_LEN as u32)
        || labels > MAX_ESSENCE_CONTAINER_LABELS
        || labels.checked_mul(KEY_LEN).is_none_or(|n| n > batch_bytes)
    {
        return Err(corrupt("mxf-essence-container-batch"));
    }
    let mut operational_pattern = [0u8; KEY_LEN];
    operational_pattern.copy_from_slice(&v[64..80]);
    let essence_containers = (0..labels)
        .map(|i| {
            let at = PACK_FIXED.saturating_add(i.saturating_mul(KEY_LEN));
            let mut label = [0u8; KEY_LEN];
            label.copy_from_slice(&v[at..at.saturating_add(KEY_LEN)]);
            label
        })
        .collect();
    Ok(MxfPartition {
        kind,
        status,
        offset: pos,
        major_version: u16_at(0),
        minor_version: u16_at(2),
        kag_size: u32_at(4),
        this_partition: u64_at(8),
        previous_partition: u64_at(16),
        footer_partition: u64_at(24),
        header_byte_count: u64_at(32),
        index_byte_count: u64_at(40),
        index_sid: u32_at(48),
        body_offset: u64_at(52),
        body_sid: u32_at(60),
        operational_pattern,
        essence_containers,
        // Filled by `layout_partitions` once the next partition is known.
        pack_end: klv.end(),
        header_start: klv.end(),
        index_start: klv.end(),
        essence_start: None,
        essence_first_klv: None,
        end: file_len,
    })
}

/// Footer pack offset (absolute), when the header pack names one that lies inside the source.
pub(super) fn absolute(base: u64, relative: u64, file_len: u64) -> Option<u64> {
    base.checked_add(relative).filter(|abs| *abs < file_len)
}
