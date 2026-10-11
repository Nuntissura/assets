//! Own Matroska / WebM (EBML) demux index: EBML header, Segment (known or unknown size), Info
//! (TimestampScale, Duration), Tracks, Cluster (Timestamp, SimpleBlock, BlockGroup/Block/BlockDuration/
//! ReferenceBlock), Cues, with Xiph / EBML / fixed-size lacing. Element IDs follow the IANA Matroska
//! registry; block and lacing layouts follow RFC 9559 section 10, EBML coding RFC 8794.
//!
//! The index reuses `TrackIndex` (exact PTS in Pulse ticks, frame-accurate seek, pre-roll from the
//! nearest earlier keyframe, packet delivery), so the same hostile-input rules apply: every size is
//! bounded before allocation, every sample spends the shared budget, cancel is polled. Compressed codecs
//! are NOT decoded (`CodecUnavailable` naming the CodecID); tracks with `ContentEncodings` (header
//! stripping, compression, encryption) are typed `Unsupported` at delivery. `cued_seek` finds the start
//! cluster through `Cues` and scans only the clusters it needs.
use crate::error::{ReelError, check_cancel};
use crate::isobmff::{
    AudioFormat, ExternalTrack, SampleEntry, SeekResult, TrackIndex, TrackKind, deliver_packets,
};
use crate::model::*;
use crate::video::{VideoCodec, VideoRoute, VideoTrack};
use hsk_studio_accord::{CancellationToken, Rounding, Ticks, Timescale};
use std::io::{Read, Seek, SeekFrom};

pub const MATROSKA_PROVIDER_ID: &str = "matroska";
const DECODER_VERSION: &str = "1";

const ID_EBML: u32 = 0x1A45_DFA3;
const ID_DOCTYPE: u32 = 0x4282;
const ID_MAX_ID_LENGTH: u32 = 0x42F2;
const ID_MAX_SIZE_LENGTH: u32 = 0x42F3;
const ID_SEGMENT: u32 = 0x1853_8067;
const ID_SEEKHEAD: u32 = 0x114D_9B74;
const ID_INFO: u32 = 0x1549_A966;
const ID_TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
const ID_DURATION: u32 = 0x4489;
const ID_TRACKS: u32 = 0x1654_AE6B;
const ID_TRACK_ENTRY: u32 = 0xAE;
const ID_TRACK_NUMBER: u32 = 0xD7;
const ID_TRACK_UID: u32 = 0x73C5;
const ID_TRACK_TYPE: u32 = 0x83;
const ID_FLAG_ENABLED: u32 = 0xB9;
const ID_FLAG_DEFAULT: u32 = 0x88;
const ID_FLAG_LACING: u32 = 0x9C;
const ID_DEFAULT_DURATION: u32 = 0x23_E383;
const ID_CODEC_ID: u32 = 0x86;
const ID_CODEC_PRIVATE: u32 = 0x63A2;
const ID_CODEC_DELAY: u32 = 0x56AA;
const ID_SEEK_PRE_ROLL: u32 = 0x56BB;
const ID_VIDEO: u32 = 0xE0;
const ID_PIXEL_WIDTH: u32 = 0xB0;
const ID_PIXEL_HEIGHT: u32 = 0xBA;
const ID_AUDIO: u32 = 0xE1;
const ID_SAMPLING_FREQUENCY: u32 = 0xB5;
const ID_CHANNELS: u32 = 0x9F;
const ID_BIT_DEPTH: u32 = 0x6264;
const ID_CONTENT_ENCODINGS: u32 = 0x6D80;
const ID_CLUSTER: u32 = 0x1F43_B675;
const ID_TIMESTAMP: u32 = 0xE7;
const ID_SIMPLE_BLOCK: u32 = 0xA3;
const ID_BLOCK_GROUP: u32 = 0xA0;
const ID_BLOCK: u32 = 0xA1;
const ID_BLOCK_DURATION: u32 = 0x9B;
const ID_REFERENCE_BLOCK: u32 = 0xFB;
const ID_CUES: u32 = 0x1C53_BB6B;
const ID_CUE_POINT: u32 = 0xBB;
const ID_CUE_TIME: u32 = 0xB3;
const ID_CUE_TRACK_POSITIONS: u32 = 0xB7;
const ID_CUE_TRACK: u32 = 0xF7;
const ID_CUE_CLUSTER_POSITION: u32 = 0xF1;
const ID_CUE_RELATIVE_POSITION: u32 = 0xF0;
const ID_CUE_DURATION: u32 = 0xB2;
const ID_CUE_BLOCK_NUMBER: u32 = 0x5378;
const ID_VOID: u32 = 0xEC;
const ID_CRC32: u32 = 0xBF;
const ID_TAGS: u32 = 0x1254_C367;
const ID_CHAPTERS: u32 = 0x1043_A770;
const ID_ATTACHMENTS: u32 = 0x1941_A469;

const MAX_LEVEL1_ELEMENTS: u32 = 1 << 20;
const MAX_UNKNOWN_CLUSTER_CHILDREN: u32 = 1 << 24;
const MAX_CODEC_PRIVATE: usize = 1 << 20;
const MAX_EBML_HEADER: u64 = 4096;
const LACE_PEEK: u64 = 65_536;
const PEEK: usize = 12;
const DEFAULT_TIMESTAMP_SCALE_NS: u64 = 1_000_000;
/// Largest declared picture side accepted (16K video is 16_384). Sizes feed width x height x planes x
/// bytes arithmetic downstream (decoder caps, frame budgets): bounding them here keeps every such
/// product far inside u64 and stops a hostile `PixelWidth` from becoming a wrapped or panicking size.
const MAX_VIDEO_DIMENSION: u64 = 1 << 20;

#[derive(Debug, Clone, Copy)]
pub struct MatroskaProvider;

// ---- EBML primitives ----------------------------------------------------------------------------

struct Head {
    id: u32,
    /// `None` = the reserved all-ones "unknown size".
    size: Option<u64>,
    header_len: u64,
}

fn corrupt(what: &'static str) -> ReelError {
    ReelError::Corrupt { what }
}

fn parse_head(buf: &[u8]) -> Result<Head, ReelError> {
    let first = *buf.first().ok_or(corrupt("ebml-header-truncated"))?;
    if first == 0 {
        return Err(corrupt("ebml-id-length"));
    }
    let id_len = first.leading_zeros() as usize + 1;
    if id_len > 4 {
        return Err(corrupt("ebml-id-length"));
    }
    let size_first = *buf.get(id_len).ok_or(corrupt("ebml-header-truncated"))?;
    if size_first == 0 {
        return Err(corrupt("ebml-size-length"));
    }
    let size_len = size_first.leading_zeros() as usize + 1;
    if buf.len() < id_len + size_len {
        return Err(corrupt("ebml-header-truncated"));
    }
    let id = buf[..id_len]
        .iter()
        .fold(0u32, |acc, b| (acc << 8) | u32::from(*b));
    let mut value = u64::from(u16::from(size_first) & (0xFFu16 >> size_len));
    for b in &buf[id_len + 1..id_len + size_len] {
        value = (value << 8) | u64::from(*b);
    }
    let unknown = value == (1u64 << (7 * size_len)) - 1;
    Ok(Head {
        id,
        size: (!unknown).then_some(value),
        header_len: (id_len + size_len) as u64,
    })
}

fn peek_head(src: &mut dyn ReadSeek, pos: u64, end: u64) -> Result<Head, ReelError> {
    let n = end.saturating_sub(pos).min(PEEK as u64) as usize;
    let mut buf = [0u8; PEEK];
    src.seek(SeekFrom::Start(pos))?;
    read_exact_structural(src, &mut buf[..n], "ebml-header-truncated")?;
    parse_head(&buf[..n])
}

fn read_at(
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

fn uint(payload: &[u8]) -> Result<u64, ReelError> {
    if payload.len() > 8 {
        return Err(corrupt("ebml-uint"));
    }
    Ok(payload.iter().fold(0u64, |a, b| (a << 8) | u64::from(*b)))
}

fn float(payload: &[u8]) -> Result<f64, ReelError> {
    match payload.len() {
        0 => Ok(0.0),
        4 => Ok(f64::from(f32::from_bits(u32::from_be_bytes([
            payload[0], payload[1], payload[2], payload[3],
        ])))),
        8 => {
            let mut b = [0u8; 8];
            b.copy_from_slice(payload);
            Ok(f64::from_bits(u64::from_be_bytes(b)))
        }
        _ => Err(corrupt("ebml-float")),
    }
}

/// Children of an in-memory master element. Unknown-size or overlong children are `Corrupt`.
struct Elems<'a> {
    data: &'a [u8],
    pos: usize,
}

fn elems(data: &[u8]) -> Elems<'_> {
    Elems { data, pos: 0 }
}

impl<'a> Iterator for Elems<'a> {
    type Item = Result<(u32, &'a [u8]), ReelError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.data.len() {
            return None;
        }
        let data: &'a [u8] = self.data;
        let rest = &data[self.pos..];
        let result: Result<(u32, &'a [u8], usize), ReelError> = (|| {
            let head = parse_head(&rest[..rest.len().min(PEEK)])?;
            let size = head.size.ok_or(corrupt("unknown-size-child"))?;
            let end = usize::try_from(head.header_len)
                .ok()
                .zip(usize::try_from(size).ok())
                .and_then(|(h, s)| h.checked_add(s))
                .filter(|end| *end <= rest.len())
                .ok_or(corrupt("element-exceeds-parent"))?;
            Ok((head.id, &rest[head.header_len as usize..end], end))
        })();
        match result {
            Ok((id, payload, end)) => {
                self.pos += end;
                Some(Ok((id, payload)))
            }
            Err(e) => {
                self.pos = self.data.len();
                Some(Err(e))
            }
        }
    }
}

/// A declared `PixelWidth` / `PixelHeight`: bounded, never truncated (a value above u32::MAX used to
/// become 0, "undeclared").
fn video_dimension(value: u64) -> Result<u32, ReelError> {
    if value > MAX_VIDEO_DIMENSION {
        return Err(ReelError::LimitExceeded {
            what: "video-dimensions",
        });
    }
    u32::try_from(value).map_err(|_| ReelError::LimitExceeded {
        what: "video-dimensions",
    })
}

/// Unsigned vint inside a lace header.
fn lace_vint(buf: &[u8], pos: &mut usize) -> Result<(u64, usize), ReelError> {
    let first = *buf.get(*pos).ok_or(corrupt("lace-header-truncated"))?;
    if first == 0 {
        return Err(corrupt("lace-vint"));
    }
    let len = first.leading_zeros() as usize + 1;
    if *pos + len > buf.len() {
        return Err(corrupt("lace-header-truncated"));
    }
    let mut value = u64::from(u16::from(first) & (0xFFu16 >> len));
    for b in &buf[*pos + 1..*pos + len] {
        value = (value << 8) | u64::from(*b);
    }
    *pos += len;
    Ok((value, len))
}

// ---- layout ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct MkvTrackInfo {
    pub number: u32,
    pub uid: u64,
    /// 1 video, 2 audio, 17 subtitle, ...
    pub track_type: u8,
    pub codec_id: String,
    pub codec_private: Vec<u8>,
    pub default_duration_ns: Option<u64>,
    pub flag_lacing: bool,
    pub enabled: bool,
    pub default: bool,
    pub codec_delay_ns: u64,
    pub seek_pre_roll_ns: u64,
    /// `ContentEncodings` present (compression / header stripping / encryption): not undone here.
    pub content_encoded: bool,
    pub width: u32,
    pub height: u32,
    pub channels: u16,
    pub sampling_frequency: f64,
    pub bit_depth: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterRef {
    /// Segment-relative position of the Cluster element (what `CueClusterPosition` stores).
    pub position: u64,
    pub data_start: u64,
    pub end: u64,
    pub timecode: Option<u64>,
    /// The cluster ran past the end of the file (recording cut short).
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuePosition {
    pub track: u64,
    pub cluster_position: u64,
    pub relative_position: Option<u64>,
    pub block_number: Option<u64>,
    pub duration: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuePoint {
    /// In TimestampScale units.
    pub time: u64,
    pub positions: Vec<CuePosition>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MkvLayout {
    pub doc_type: String,
    pub segment_data_start: u64,
    pub segment_end: u64,
    pub timestamp_scale_ns: u64,
    /// Segment `Duration` in TimestampScale units.
    pub duration_units: Option<f64>,
    pub tracks: Vec<MkvTrackInfo>,
    pub clusters: Vec<ClusterRef>,
    pub cues: Vec<CuePoint>,
    pub truncated_tail: bool,
    /// Bytes read to build the layout.
    pub bytes_read: u64,
}

impl MkvLayout {
    fn media_timescale(&self) -> u32 {
        (1_000_000_000 / self.timestamp_scale_ns) as u32
    }
    pub fn track(&self, number: u32) -> Option<&MkvTrackInfo> {
        self.tracks.iter().find(|t| t.number == number)
    }
}

/// Counts the bytes read from the underlying source.
struct Counting<'a> {
    inner: &'a mut dyn ReadSeek,
    bytes: u64,
}

impl Read for Counting<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }
}

impl Seek for Counting<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

fn parse_track_entry(payload: &[u8]) -> Result<MkvTrackInfo, ReelError> {
    let mut t = MkvTrackInfo {
        number: 0,
        uid: 0,
        track_type: 0,
        codec_id: String::new(),
        codec_private: Vec::new(),
        default_duration_ns: None,
        flag_lacing: true,
        enabled: true,
        default: true,
        codec_delay_ns: 0,
        seek_pre_roll_ns: 0,
        content_encoded: false,
        width: 0,
        height: 0,
        channels: 1,
        sampling_frequency: 8000.0,
        bit_depth: 0,
    };
    for item in elems(payload) {
        let (id, p) = item?;
        match id {
            ID_TRACK_NUMBER => {
                let n = uint(p)?;
                t.number =
                    u32::try_from(n)
                        .ok()
                        .filter(|n| *n != 0)
                        .ok_or(ReelError::Unsupported {
                            what: "track-number",
                        })?;
            }
            ID_TRACK_UID => t.uid = uint(p)?,
            ID_TRACK_TYPE => t.track_type = u8::try_from(uint(p)?).unwrap_or(0),
            ID_FLAG_ENABLED => t.enabled = uint(p)? != 0,
            ID_FLAG_DEFAULT => t.default = uint(p)? != 0,
            ID_FLAG_LACING => t.flag_lacing = uint(p)? != 0,
            ID_DEFAULT_DURATION => t.default_duration_ns = Some(uint(p)?).filter(|d| *d != 0),
            ID_CODEC_ID => {
                t.codec_id = String::from_utf8(p.to_vec())
                    .map_err(|_| corrupt("codec-id"))?
                    .trim_end_matches('\0')
                    .to_owned();
            }
            ID_CODEC_PRIVATE => {
                if p.len() > MAX_CODEC_PRIVATE {
                    return Err(ReelError::LimitExceeded {
                        what: "codec-private",
                    });
                }
                t.codec_private = p.to_vec();
            }
            ID_CODEC_DELAY => t.codec_delay_ns = uint(p)?,
            ID_SEEK_PRE_ROLL => t.seek_pre_roll_ns = uint(p)?,
            ID_CONTENT_ENCODINGS => t.content_encoded = true,
            ID_VIDEO => {
                for sub in elems(p) {
                    let (sid, sp) = sub?;
                    match sid {
                        ID_PIXEL_WIDTH => t.width = video_dimension(uint(sp)?)?,
                        ID_PIXEL_HEIGHT => t.height = video_dimension(uint(sp)?)?,
                        _ => {}
                    }
                }
            }
            ID_AUDIO => {
                for sub in elems(p) {
                    let (sid, sp) = sub?;
                    match sid {
                        ID_SAMPLING_FREQUENCY => t.sampling_frequency = float(sp)?,
                        ID_CHANNELS => t.channels = u16::try_from(uint(sp)?).unwrap_or(0),
                        ID_BIT_DEPTH => t.bit_depth = u16::try_from(uint(sp)?).unwrap_or(0),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    if t.number == 0 {
        return Err(corrupt("track-number-missing"));
    }
    Ok(t)
}

fn parse_cues(payload: &[u8], limits: &Limits) -> Result<Vec<CuePoint>, ReelError> {
    let mut cues = Vec::new();
    for item in elems(payload) {
        let (id, p) = item?;
        if id != ID_CUE_POINT {
            continue;
        }
        if cues.len() >= limits.max_samples {
            return Err(ReelError::LimitExceeded { what: "cue-count" });
        }
        let (mut time, mut positions) = (None, Vec::new());
        for sub in elems(p) {
            let (sid, sp) = sub?;
            match sid {
                ID_CUE_TIME => time = Some(uint(sp)?),
                ID_CUE_TRACK_POSITIONS => {
                    let mut pos = CuePosition {
                        track: 0,
                        cluster_position: 0,
                        relative_position: None,
                        block_number: None,
                        duration: None,
                    };
                    for field in elems(sp) {
                        let (fid, fp) = field?;
                        match fid {
                            ID_CUE_TRACK => pos.track = uint(fp)?,
                            ID_CUE_CLUSTER_POSITION => pos.cluster_position = uint(fp)?,
                            ID_CUE_RELATIVE_POSITION => pos.relative_position = Some(uint(fp)?),
                            ID_CUE_BLOCK_NUMBER => pos.block_number = Some(uint(fp)?),
                            ID_CUE_DURATION => pos.duration = Some(uint(fp)?),
                            _ => {}
                        }
                    }
                    positions.push(pos);
                }
                _ => {}
            }
        }
        cues.push(CuePoint {
            time: time.ok_or(corrupt("cue-time-missing"))?,
            positions,
        });
    }
    Ok(cues)
}

fn is_level1(id: u32) -> bool {
    matches!(
        id,
        ID_SEEKHEAD
            | ID_INFO
            | ID_TRACKS
            | ID_CLUSTER
            | ID_CUES
            | ID_TAGS
            | ID_CHAPTERS
            | ID_ATTACHMENTS
    )
}

/// End of an unknown-size Cluster: the first level-1 element that follows its children.
fn unknown_cluster_end(
    src: &mut dyn ReadSeek,
    data_start: u64,
    segment_end: u64,
    cancel: &CancellationToken,
) -> Result<u64, ReelError> {
    let mut pos = data_start;
    for _ in 0..MAX_UNKNOWN_CLUSTER_CHILDREN {
        check_cancel(cancel)?;
        if segment_end.saturating_sub(pos) < 2 {
            return Ok(segment_end);
        }
        let head = peek_head(src, pos, segment_end)?;
        if is_level1(head.id) {
            return Ok(pos);
        }
        let size = head.size.ok_or(corrupt("unknown-size-child"))?;
        match pos
            .checked_add(head.header_len)
            .and_then(|p| p.checked_add(size))
            .filter(|next| *next <= segment_end)
        {
            Some(next) => pos = next,
            None => return Ok(segment_end),
        }
    }
    Err(ReelError::LimitExceeded {
        what: "cluster-children",
    })
}

fn first_child_timecode(
    src: &mut dyn ReadSeek,
    data_start: u64,
    end: u64,
) -> Result<Option<u64>, ReelError> {
    if end.saturating_sub(data_start) < 2 {
        return Ok(None);
    }
    let head = peek_head(src, data_start, end)?;
    match head.size {
        Some(size) if head.id == ID_TIMESTAMP && size <= 8 => {
            let at = data_start + head.header_len;
            if at.checked_add(size).is_none_or(|e| e > end) {
                return Ok(None);
            }
            Ok(Some(uint(&read_at(
                src,
                at,
                size as usize,
                "cluster-timestamp",
            )?)?))
        }
        _ => Ok(None),
    }
}

/// Read headers, Info, Tracks, Cues and the cluster table. Never reads frame payloads.
pub fn read_layout(
    src: &mut dyn ReadSeek,
    file_len: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<MkvLayout, ReelError> {
    let mut counting = Counting {
        inner: src,
        bytes: 0,
    };
    let layout = read_layout_inner(&mut counting, file_len, limits, cancel);
    let bytes = counting.bytes;
    layout.map(|mut l| {
        l.bytes_read = bytes;
        l
    })
}

fn read_layout_inner(
    src: &mut dyn ReadSeek,
    file_len: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<MkvLayout, ReelError> {
    // EBML header.
    let magic = read_at(src, 0, file_len.min(4) as usize, "ebml-magic")?;
    if magic != [0x1A, 0x45, 0xDF, 0xA3] {
        return Err(ReelError::ImportUnsupported {
            detected: "not-ebml".into(),
        });
    }
    let head = peek_head(src, 0, file_len)?;
    if head.id != ID_EBML {
        return Err(ReelError::ImportUnsupported {
            detected: "not-ebml".into(),
        });
    }
    let size = head
        .size
        .filter(|s| *s <= MAX_EBML_HEADER)
        .ok_or(corrupt("ebml-header-size"))?;
    let header_end = head.header_len + size;
    if header_end > file_len {
        return Err(corrupt("ebml-header-truncated"));
    }
    let payload = read_at(src, head.header_len, size as usize, "ebml-header")?;
    let mut doc_type = String::new();
    for item in elems(&payload) {
        let (id, p) = item?;
        match id {
            ID_DOCTYPE => doc_type = String::from_utf8_lossy(p).trim_end_matches('\0').to_owned(),
            ID_MAX_ID_LENGTH if uint(p)? > 4 => {
                return Err(ReelError::Unsupported {
                    what: "ebml-max-id-length",
                });
            }
            ID_MAX_SIZE_LENGTH if uint(p)? > 8 => {
                return Err(ReelError::Unsupported {
                    what: "ebml-max-size-length",
                });
            }
            _ => {}
        }
    }
    if doc_type != "matroska" && doc_type != "webm" {
        return Err(ReelError::ImportUnsupported {
            detected: format!("ebml:{doc_type}"),
        });
    }

    // Segment (skipping Void / CRC-32 before it).
    let mut pos = header_end;
    let (segment_head, segment_pos) = loop {
        if file_len.saturating_sub(pos) < 2 {
            return Err(corrupt("segment-missing"));
        }
        let h = peek_head(src, pos, file_len)?;
        match h.id {
            ID_SEGMENT => break (h, pos),
            ID_VOID | ID_CRC32 => {
                let n = h.size.ok_or(corrupt("unknown-size-element"))?;
                pos = pos
                    .checked_add(h.header_len)
                    .and_then(|p| p.checked_add(n))
                    .ok_or(corrupt("element-exceeds-parent"))?;
            }
            _ => return Err(corrupt("segment-missing")),
        }
    };
    let data_start = segment_pos + segment_head.header_len;
    let mut truncated_tail = false;
    let segment_end = match segment_head.size {
        Some(n) => match data_start.checked_add(n) {
            Some(end) if end <= file_len => end,
            _ => {
                truncated_tail = true;
                file_len
            }
        },
        None => file_len,
    };

    let mut layout = MkvLayout {
        doc_type,
        segment_data_start: data_start,
        segment_end,
        timestamp_scale_ns: DEFAULT_TIMESTAMP_SCALE_NS,
        duration_units: None,
        tracks: Vec::new(),
        clusters: Vec::new(),
        cues: Vec::new(),
        truncated_tail: false,
        bytes_read: 0,
    };
    let mut probed = header_end;
    pos = data_start;
    for _ in 0..MAX_LEVEL1_ELEMENTS {
        check_cancel(cancel)?;
        if segment_end.saturating_sub(pos) < 2 {
            break;
        }
        let h = peek_head(src, pos, segment_end)?;
        let body = pos + h.header_len;
        match h.id {
            ID_CLUSTER => {
                let (end, truncated) = match h.size {
                    Some(n) => match body.checked_add(n) {
                        Some(end) if end <= segment_end => (end, false),
                        _ => {
                            truncated_tail = true;
                            (segment_end, true)
                        }
                    },
                    None => (unknown_cluster_end(src, body, segment_end, cancel)?, false),
                };
                if layout.clusters.len() >= limits.max_samples {
                    return Err(ReelError::LimitExceeded {
                        what: "cluster-count",
                    });
                }
                layout.clusters.push(ClusterRef {
                    position: pos - data_start,
                    data_start: body,
                    end,
                    timecode: first_child_timecode(src, body, end)?,
                    truncated,
                });
                pos = end;
            }
            ID_INFO | ID_TRACKS | ID_CUES => {
                let n = h.size.ok_or(corrupt("unknown-size-element"))?;
                let Some(end) = body.checked_add(n).filter(|e| *e <= segment_end) else {
                    truncated_tail = true;
                    break;
                };
                if n > limits.max_probe_bytes.saturating_sub(probed) {
                    return Err(ReelError::LimitExceeded {
                        what: "probe-bytes",
                    });
                }
                probed += n;
                let payload = read_at(src, body, n as usize, "segment-element")?;
                match h.id {
                    ID_INFO => {
                        for item in elems(&payload) {
                            let (id, p) = item?;
                            match id {
                                ID_TIMESTAMP_SCALE => layout.timestamp_scale_ns = uint(p)?,
                                ID_DURATION => layout.duration_units = Some(float(p)?),
                                _ => {}
                            }
                        }
                    }
                    ID_TRACKS => {
                        for item in elems(&payload) {
                            let (id, p) = item?;
                            if id != ID_TRACK_ENTRY {
                                continue;
                            }
                            if layout.tracks.len() >= limits.max_tracks {
                                return Err(ReelError::LimitExceeded {
                                    what: "track-count",
                                });
                            }
                            let track = parse_track_entry(p)?;
                            if layout.track(track.number).is_some() {
                                return Err(corrupt("duplicate-track-number"));
                            }
                            layout.tracks.push(track);
                        }
                    }
                    _ => layout.cues = parse_cues(&payload, limits)?,
                }
                pos = end;
            }
            _ => {
                let n = h.size.ok_or(corrupt("unknown-size-element"))?;
                match body.checked_add(n).filter(|e| *e <= segment_end) {
                    Some(end) => pos = end,
                    None => {
                        truncated_tail = true;
                        break;
                    }
                }
            }
        }
    }
    let scale = layout.timestamp_scale_ns;
    if scale == 0 || !1_000_000_000u64.is_multiple_of(scale) {
        return Err(ReelError::Unsupported {
            what: "timestamp-scale-not-a-divisor-of-1e9",
        });
    }
    if layout.tracks.is_empty() {
        return Err(corrupt("tracks-missing"));
    }
    layout.truncated_tail = truncated_tail || layout.clusters.iter().any(|c| c.truncated);
    Ok(layout)
}

// ---- blocks ----------------------------------------------------------------------------------------

struct ParsedFrame {
    track: usize,
    pts: i64,
    offset: u64,
    size: u32,
    /// TimestampScale units; 0 = unknown.
    duration: u32,
    key: bool,
}

fn default_units(track: &MkvTrackInfo, scale_ns: u64) -> u32 {
    track
        .default_duration_ns
        .filter(|d| d.is_multiple_of(scale_ns))
        .map_or(0, |d| u32::try_from(d / scale_ns).unwrap_or(0))
}

/// Parse one Block / SimpleBlock payload at `data_pos` into its frames (lacing expanded).
fn parse_block(
    src: &mut dyn ReadSeek,
    data_pos: u64,
    size: u64,
    cluster_timecode: Option<u64>,
    tracks: &[MkvTrackInfo],
    scale_ns: u64,
) -> Result<Vec<ParsedFrame>, ReelError> {
    let tc = cluster_timecode.ok_or(corrupt("block-before-cluster-timestamp"))?;
    if size < 4 {
        return Err(corrupt("block-header"));
    }
    let buf = read_at(src, data_pos, size.min(16) as usize, "block-header")?;
    let first = buf[0];
    if first == 0 {
        return Err(corrupt("block-track-vint"));
    }
    let tlen = first.leading_zeros() as usize + 1;
    if tlen + 3 > buf.len() {
        return Err(corrupt("block-header"));
    }
    let mut number = u64::from(u16::from(first) & (0xFFu16 >> tlen));
    for b in &buf[1..tlen] {
        number = (number << 8) | u64::from(*b);
    }
    let rel = i16::from_be_bytes([buf[tlen], buf[tlen + 1]]);
    let flags = buf[tlen + 2];
    let header_len = (tlen + 3) as u64;
    let ti = tracks
        .iter()
        .position(|t| u64::from(t.number) == number)
        .ok_or(corrupt("block-unknown-track"))?;
    let base = i64::try_from(tc)
        .ok()
        .and_then(|t| t.checked_add(i64::from(rel)))
        .ok_or(ReelError::LimitExceeded { what: "time-range" })?;
    let payload_len = size - header_len;
    let key = flags & 0x80 != 0;
    let lacing = (flags & 0x06) >> 1;
    let to_u32 = |n: u64| {
        u32::try_from(n).map_err(|_| ReelError::LimitExceeded {
            what: "frame-bytes",
        })
    };
    if lacing == 0 {
        return Ok(vec![ParsedFrame {
            track: ti,
            pts: base,
            offset: data_pos + header_len,
            size: to_u32(payload_len)?,
            duration: default_units(&tracks[ti], scale_ns),
            key,
        }]);
    }
    if !tracks[ti].flag_lacing {
        return Err(corrupt("lacing-on-unlaced-track"));
    }
    let head_bytes = read_at(
        src,
        data_pos + header_len,
        payload_len.min(LACE_PEEK) as usize,
        "lace-header",
    )?;
    let count = usize::from(*head_bytes.first().ok_or(corrupt("lace-header-truncated"))?) + 1;
    let mut p = 1usize;
    let mut sizes: Vec<u64> = Vec::with_capacity(count);
    match lacing {
        1 => {
            for _ in 0..count - 1 {
                let mut total = 0u64;
                loop {
                    let b = *head_bytes.get(p).ok_or(corrupt("lace-header-truncated"))?;
                    p += 1;
                    total += u64::from(b);
                    if b != 255 {
                        break;
                    }
                }
                sizes.push(total);
            }
        }
        3 => {
            if count >= 2 {
                let (first_size, _) = lace_vint(&head_bytes, &mut p)?;
                sizes.push(first_size);
                for _ in 1..count - 1 {
                    let (raw, len) = lace_vint(&head_bytes, &mut p)?;
                    let delta = raw as i64 - ((1i64 << (7 * len - 1)) - 1);
                    let prev = *sizes.last().unwrap_or(&0) as i64;
                    let next = prev
                        .checked_add(delta)
                        .filter(|n| *n >= 0)
                        .ok_or(corrupt("lace-size-negative"))?;
                    sizes.push(next as u64);
                }
            }
        }
        _ => {
            let body = payload_len
                .checked_sub(1)
                .ok_or(corrupt("lace-header-truncated"))?;
            if body % count as u64 != 0 {
                return Err(corrupt("fixed-lace-size"));
            }
            sizes.extend(std::iter::repeat_n(body / count as u64, count - 1));
        }
    }
    let rest = payload_len
        .checked_sub(p as u64)
        .ok_or(corrupt("lace-header-truncated"))?;
    let used = sizes
        .iter()
        .try_fold(0u64, |acc, size| acc.checked_add(*size))
        .ok_or(corrupt("lace-sizes-exceed-block"))?;
    let last = rest
        .checked_sub(used)
        .ok_or(corrupt("lace-sizes-exceed-block"))?;
    sizes.push(last);
    let default = tracks[ti]
        .default_duration_ns
        .ok_or(ReelError::Unsupported {
            what: "lacing-without-default-duration",
        })?;
    if default % scale_ns != 0 {
        return Err(ReelError::Unsupported {
            what: "laced-frame-duration-not-a-timestamp-multiple",
        });
    }
    let step = i64::try_from(default / scale_ns)
        .map_err(|_| ReelError::LimitExceeded { what: "time-range" })?;
    let mut offset = data_pos + header_len + p as u64;
    let mut frames = Vec::with_capacity(count);
    for (i, frame_size) in sizes.iter().enumerate() {
        frames.push(ParsedFrame {
            track: ti,
            pts: base
                .checked_add(step.saturating_mul(i as i64))
                .ok_or(ReelError::LimitExceeded { what: "time-range" })?,
            offset,
            size: to_u32(*frame_size)?,
            duration: u32::try_from(step).unwrap_or(0),
            key,
        });
        offset += frame_size;
    }
    Ok(frames)
}

/// Scan one cluster's blocks, appending `(track index, sample)` for the wanted tracks.
#[allow(clippy::too_many_arguments)]
fn scan_cluster(
    src: &mut dyn ReadSeek,
    cluster: &ClusterRef,
    tracks: &[MkvTrackInfo],
    scale_ns: u64,
    only_track: Option<usize>,
    cancel: &CancellationToken,
    budget: &mut usize,
    out: &mut Vec<(usize, SampleEntry)>,
) -> Result<(), ReelError> {
    let (mut timecode, mut pos) = (cluster.timecode, cluster.data_start);
    let mut push = |frames: Vec<ParsedFrame>,
                    key_override: Option<bool>,
                    block_duration: Option<u64>,
                    budget: &mut usize|
     -> Result<(), ReelError> {
        let single = frames.len() == 1;
        for f in frames {
            if only_track.is_some_and(|t| t != f.track) {
                continue;
            }
            if *budget == 0 {
                return Err(ReelError::LimitExceeded {
                    what: "sample-count",
                });
            }
            *budget -= 1;
            let duration = match block_duration {
                Some(d) if single => u32::try_from(d).unwrap_or(0),
                _ => f.duration,
            };
            out.push((
                f.track,
                SampleEntry {
                    dts: f.pts,
                    pts: f.pts,
                    duration,
                    offset: f.offset,
                    size: f.size,
                    sync: key_override.unwrap_or(f.key),
                },
            ));
        }
        Ok(())
    };
    while cluster.end.saturating_sub(pos) >= 2 {
        check_cancel(cancel)?;
        let h = peek_head(src, pos, cluster.end)?;
        let body = pos + h.header_len;
        let size = h.size.ok_or(corrupt("unknown-size-child"))?;
        let Some(end) = body.checked_add(size).filter(|e| *e <= cluster.end) else {
            if cluster.truncated {
                break; // the recording was cut inside this element
            }
            return Err(corrupt("element-exceeds-parent"));
        };
        match h.id {
            ID_TIMESTAMP => {
                if size > 8 {
                    return Err(corrupt("ebml-uint"));
                }
                timecode = Some(uint(&read_at(
                    src,
                    body,
                    size as usize,
                    "cluster-timestamp",
                )?)?);
            }
            ID_SIMPLE_BLOCK => {
                let frames = parse_block(src, body, size, timecode, tracks, scale_ns)?;
                push(frames, None, None, budget)?;
            }
            ID_BLOCK_GROUP => {
                let (mut gpos, mut frames) = (body, Vec::new());
                let (mut duration, mut referenced) = (None, false);
                while end.saturating_sub(gpos) >= 2 {
                    let gh = peek_head(src, gpos, end)?;
                    let gbody = gpos + gh.header_len;
                    let gsize = gh.size.ok_or(corrupt("unknown-size-child"))?;
                    let gend = gbody
                        .checked_add(gsize)
                        .filter(|e| *e <= end)
                        .ok_or(corrupt("element-exceeds-parent"))?;
                    match gh.id {
                        ID_BLOCK => {
                            frames = parse_block(src, gbody, gsize, timecode, tracks, scale_ns)?;
                        }
                        ID_BLOCK_DURATION if gsize <= 8 => {
                            duration = Some(uint(&read_at(
                                src,
                                gbody,
                                gsize as usize,
                                "block-duration",
                            )?)?);
                        }
                        ID_REFERENCE_BLOCK => referenced = true,
                        _ => {}
                    }
                    gpos = gend;
                }
                push(frames, Some(!referenced), duration, budget)?;
            }
            _ => {}
        }
        pos = end;
    }
    Ok(())
}

/// Fill unknown durations from the next presented frame; the last frame uses the segment end, else the
/// previous frame's duration.
fn finish_durations(samples: &mut [SampleEntry], end_hint: Option<i64>) {
    let mut order: Vec<usize> = (0..samples.len()).collect();
    order.sort_by_key(|&i| samples[i].pts);
    for w in 0..order.len() {
        let i = order[w];
        if samples[i].duration != 0 {
            continue;
        }
        let pts = samples[i].pts;
        let next = order
            .get(w + 1)
            .and_then(|&j| samples[j].pts.checked_sub(pts))
            .filter(|d| *d > 0);
        let fallback = || {
            end_hint
                .and_then(|e| e.checked_sub(pts))
                .filter(|d| *d > 0)
                .or_else(|| {
                    w.checked_sub(1)
                        .map(|p| i64::from(samples[order[p]].duration))
                })
                .unwrap_or(0)
        };
        let d = next.unwrap_or_else(fallback);
        samples[i].duration = u32::try_from(d).unwrap_or(u32::MAX);
    }
}

fn external_track(info: &MkvTrackInfo, layout: &MkvLayout) -> ExternalTrack {
    let (kind, handler) = match info.track_type {
        1 => (TrackKind::Video, *b"vide"),
        2 => (TrackKind::Audio, *b"soun"),
        17 => (TrackKind::Other, *b"subt"),
        _ => (TrackKind::Other, *b"data"),
    };
    let rate = info.sampling_frequency;
    let integral =
        rate.is_finite() && rate >= 1.0 && rate <= f64::from(u32::MAX) && rate.fract() == 0.0;
    ExternalTrack {
        track_id: info.number,
        kind,
        handler,
        enabled: info.enabled,
        media_timescale: layout.media_timescale(),
        media_duration: layout.duration_units.map_or(0, |d| d.max(0.0) as u64),
        width: info.width,
        height: info.height,
        audio: (kind == TrackKind::Audio).then_some(AudioFormat {
            channels: info.channels,
            sample_size: info.bit_depth,
            sample_rate: if integral { rate as u32 } else { 0 },
            version: 0,
            compression_id: 0,
            lpcm: None,
        }),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MkvTrack {
    pub info: MkvTrackInfo,
    pub index: TrackIndex,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MkvIndex {
    pub layout: MkvLayout,
    pub tracks: Vec<MkvTrack>,
    /// Length of the source the index was built from; delivery re-checks it.
    pub source_len: u64,
    /// Layout bytes plus every block header read while scanning clusters.
    pub bytes_read: u64,
}

impl MkvIndex {
    pub fn track(&self, number: u32) -> Option<&MkvTrack> {
        self.tracks.iter().find(|t| t.info.number == number)
    }
    /// First video track, else the first audio track.
    pub fn default_track(&self) -> Option<&MkvTrack> {
        self.tracks
            .iter()
            .find(|t| t.info.track_type == 1)
            .or_else(|| self.tracks.iter().find(|t| t.info.track_type == 2))
    }
}

fn end_hint(layout: &MkvLayout) -> Option<i64> {
    layout
        .duration_units
        .filter(|d| d.is_finite() && *d >= 0.0 && *d < i64::MAX as f64)
        .map(|d| d as i64)
}

/// Full index: layout plus every block of every cluster.
pub fn build_matroska_index(
    src: &mut dyn ReadSeek,
    file_len: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<MkvIndex, ReelError> {
    let layout = read_layout(src, file_len, limits, cancel)?;
    let mut counting = Counting {
        inner: src,
        bytes: 0,
    };
    let mut budget = limits.max_samples;
    let mut per_track: Vec<Vec<SampleEntry>> = vec![Vec::new(); layout.tracks.len()];
    let mut scanned = Vec::new();
    for cluster in &layout.clusters {
        scanned.clear();
        scan_cluster(
            &mut counting,
            cluster,
            &layout.tracks,
            layout.timestamp_scale_ns,
            None,
            cancel,
            &mut budget,
            &mut scanned,
        )?;
        for (track, sample) in scanned.drain(..) {
            per_track[track].push(sample);
        }
    }
    let mut tracks = Vec::with_capacity(layout.tracks.len());
    for (info, mut samples) in layout.tracks.iter().zip(per_track) {
        finish_durations(&mut samples, end_hint(&layout));
        let index = TrackIndex::from_external_samples(
            external_track(info, &layout),
            samples,
            limits,
            cancel,
        )?;
        tracks.push(MkvTrack {
            info: info.clone(),
            index,
        });
    }
    let bytes_read = layout.bytes_read + counting.bytes;
    Ok(MkvIndex {
        layout,
        tracks,
        source_len: file_len,
        bytes_read,
    })
}

// ---- cue-based seek -------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct CuedSeek {
    /// Indices inside the partial sample list; use the `*_offset` fields for file positions.
    pub seek: SeekResult,
    /// The cue the scan started from (`None`: no cue at or before the target, scan from the start).
    pub cue_time: Option<u64>,
    pub clusters_scanned: u32,
    pub frames_scanned: u64,
    /// File offset of the frame shown at the requested time and of the frame to start decoding from.
    pub target_offset: u64,
    pub decode_from_offset: u64,
    pub bytes_read: u64,
}

/// Frame-accurate seek that uses `Cues` to skip clusters: scan from the cue's cluster through the first
/// cluster that starts after the target (reordered frames may be stored there), then resolve the frame
/// and the decode start exactly as a full index would.
pub fn cued_seek(
    src: &mut dyn ReadSeek,
    layout: &MkvLayout,
    track_number: u32,
    requested: Ticks,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<CuedSeek, ReelError> {
    let info = layout.track(track_number).ok_or(ReelError::Unsupported {
        what: "track-not-found",
    })?;
    let track_index = layout
        .tracks
        .iter()
        .position(|t| t.number == track_number)
        .ok_or(ReelError::Unsupported {
            what: "track-not-found",
        })?;
    let target_units = Timescale::new(layout.media_timescale())?
        .units_from_ticks(requested, Rounding::Floor)?
        .value;
    let cue = layout
        .cues
        .iter()
        .filter(|c| c.time <= target_units)
        .filter_map(|c| {
            c.positions
                .iter()
                .find(|p| p.track == u64::from(track_number))
                .map(|p| (c.time, p.cluster_position))
        })
        .max_by_key(|(time, _)| *time);
    let start = match cue {
        Some((_, position)) => layout
            .clusters
            .iter()
            .position(|c| c.position == position)
            .ok_or(corrupt("cue-cluster-not-found"))?,
        None => 0,
    };
    let mut last = layout.clusters.len().saturating_sub(1);
    for (i, c) in layout.clusters.iter().enumerate().skip(start) {
        if c.timecode.is_some_and(|t| t > target_units) {
            last = i;
            break;
        }
    }
    let scan_count = if layout.clusters.is_empty() {
        0
    } else {
        last - start + 1
    };
    let mut counting = Counting {
        inner: src,
        bytes: 0,
    };
    let mut budget = limits.max_samples;
    let mut scanned = Vec::new();
    for cluster in layout.clusters.iter().skip(start).take(scan_count) {
        scan_cluster(
            &mut counting,
            cluster,
            &layout.tracks,
            layout.timestamp_scale_ns,
            Some(track_index),
            cancel,
            &mut budget,
            &mut scanned,
        )?;
    }
    let mut samples: Vec<SampleEntry> = scanned.into_iter().map(|(_, s)| s).collect();
    let frames_scanned = samples.len() as u64;
    finish_durations(&mut samples, end_hint(layout));
    let partial = TrackIndex::from_external_samples(
        external_track(info, layout),
        samples.clone(),
        limits,
        cancel,
    )?;
    let seek = partial.seek(requested)?;
    Ok(CuedSeek {
        target_offset: samples[seek.target.sample as usize].offset,
        decode_from_offset: samples[seek.decode_from.sample as usize].offset,
        seek,
        cue_time: cue.map(|(time, _)| time),
        clusters_scanned: scan_count as u32,
        frames_scanned,
        bytes_read: counting.bytes,
    })
}

// ---- provider ---------------------------------------------------------------------------------------

fn select_track(index: &MkvIndex, wanted: Option<u32>) -> Result<&MkvTrack, ReelError> {
    match wanted {
        Some(number) => index.track(number).ok_or(ReelError::Unsupported {
            what: "track-not-found",
        }),
        None => index.default_track().ok_or(ReelError::Unsupported {
            what: "no-audio-or-video-track",
        }),
    }
}

impl MatroskaProvider {
    /// Packet delivery over a prebuilt index. The grant is verified against the live source and the
    /// index's source length before any payload read; `bytes_read` counts payload only.
    pub fn decode_packets(
        &self,
        src: &mut dyn ReadSeek,
        index: &MkvIndex,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
    ) -> Result<SegmentReceipt, ReelError> {
        check_cancel(cancel)?;
        if req.codec != MATROSKA_PROVIDER_ID {
            return Err(ReelError::CodecUnavailable {
                codec: req.codec.clone(),
            });
        }
        if req.count == 0 {
            return Err(ReelError::Unsupported { what: "zero-count" });
        }
        let len = verify_grant(src, &req.grant, req.expected_revision)?;
        if len != index.source_len {
            return Err(ReelError::StaleGrant);
        }
        self.deliver(
            src,
            index,
            req,
            limits,
            cancel,
            sink,
            &VideoRoute::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn deliver(
        &self,
        src: &mut dyn ReadSeek,
        index: &MkvIndex,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
        route: &VideoRoute<'_>,
    ) -> Result<SegmentReceipt, ReelError> {
        let track = select_track(index, req.track)?;
        if req.output == OutputMode::Decoded {
            if track.info.codec_id == "V_AV1" {
                return crate::video::decode_video_track(
                    src,
                    VideoTrack {
                        track: &track.index,
                        codec: VideoCodec::Av1,
                        container: "matroska",
                        demux_id: "matroska-demux",
                        config: &track.info.codec_private,
                        color: crate::isobmff::ColorTag::Untagged,
                        codec_label: track.info.codec_id.clone(),
                        width: track.info.width,
                        height: track.info.height,
                        // Reported only once a decoder exists: without one the track is simply unavailable.
                        unsupported: track.info.content_encoded.then_some("content-encoding"),
                    },
                    req,
                    limits,
                    cancel,
                    sink,
                    route,
                );
            }
            // No other decoder is registered for any Matroska CodecID in this slice.
            return Err(ReelError::CodecUnavailable {
                codec: track.info.codec_id.clone(),
            });
        }
        if track.info.content_encoded {
            return Err(ReelError::Unsupported {
                what: "content-encoding",
            });
        }
        deliver_packets(
            src,
            &track.index,
            track.info.codec_id.clone(),
            "matroska-demux",
            req,
            limits,
            cancel,
            sink,
        )
    }
}

impl MediaProvider for MatroskaProvider {
    fn descriptor(&self) -> CodecDescriptor {
        CodecDescriptor {
            id: MATROSKA_PROVIDER_ID,
            kind: MediaKind::Container,
            container: "matroska-webm",
            bit_depths: &[],
            chroma: None,
            transfer: None,
            decoder_version: DECODER_VERSION,
            can_decode: false,
            can_encode: false,
        }
    }

    fn sniff(&self, head: &[u8]) -> bool {
        head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
    }

    fn probe(
        &self,
        src: &mut dyn ReadSeek,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<StreamLayout, ReelError> {
        let len = src.seek(SeekFrom::End(0))?;
        let index = build_matroska_index(src, len, limits, cancel)?;
        let track = index.default_track().ok_or(ReelError::Unsupported {
            what: "no-audio-or-video-track",
        })?;
        let presented = track.index.presented();
        Ok(StreamLayout {
            kind: if track.info.track_type == 1 {
                MediaKind::Video
            } else {
                MediaKind::Audio
            },
            codec: track.info.codec_id.clone(),
            width: track.info.width,
            height: track.info.height,
            ticks_per_unit: presented.first().map_or(0, |p| p.duration),
            units: presented.len() as u64,
            channels: track.info.channels,
            tracks: index.tracks.len() as u32,
        })
    }

    fn decode_segment(
        &self,
        src: &mut dyn ReadSeek,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
    ) -> Result<SegmentReceipt, ReelError> {
        self.decode_segment_routed(src, req, limits, cancel, sink, &VideoRoute::default())
    }

    fn decode_segment_routed(
        &self,
        src: &mut dyn ReadSeek,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
        route: &VideoRoute<'_>,
    ) -> Result<SegmentReceipt, ReelError> {
        check_cancel(cancel)?;
        if req.codec != MATROSKA_PROVIDER_ID {
            return Err(ReelError::CodecUnavailable {
                codec: req.codec.clone(),
            });
        }
        if req.count == 0 {
            return Err(ReelError::Unsupported { what: "zero-count" });
        }
        let file_len = verify_grant(src, &req.grant, req.expected_revision)?;
        let index = build_matroska_index(src, file_len, limits, cancel)?;
        let mut receipt = self.deliver(src, &index, req, limits, cancel, sink, route)?;
        receipt.bytes_read += index.bytes_read;
        Ok(receipt)
    }
}
