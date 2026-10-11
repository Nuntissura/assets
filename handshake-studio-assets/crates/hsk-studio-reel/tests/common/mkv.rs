//! Synthetic Matroska/WebM builder (EBML header, Segment with Void/Info/Tracks/Clusters/Cues).
//! Written from RFC 8794 / RFC 9559 layouts, independent of the reader; returns the absolute offset of
//! every frame as an independent oracle.

fn id_bytes(id: u32) -> Vec<u8> {
    let b = id.to_be_bytes();
    let skip = b.iter().position(|x| *x != 0).unwrap_or(3);
    b[skip..].to_vec()
}

/// Shortest EBML size vint for `n` (the all-ones pattern is reserved for "unknown size").
fn size_vint(n: u64) -> Vec<u8> {
    for len in 1..=8usize {
        if n < (1u64 << (7 * len)) - 1 {
            return (n | (1u64 << (7 * len))).to_be_bytes()[8 - len..].to_vec();
        }
    }
    unreachable!("sizes are bounded by memory")
}

const UNKNOWN_SIZE: [u8; 8] = [0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];

fn element(id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = id_bytes(id);
    out.extend_from_slice(&size_vint(payload.len() as u64));
    out.extend_from_slice(payload);
    out
}

fn uint_elem(id: u32, v: u64) -> Vec<u8> {
    let bytes = v.to_be_bytes();
    let skip = bytes.iter().position(|b| *b != 0).unwrap_or(7);
    element(id, &bytes[skip..])
}

fn float_elem(id: u32, v: f64) -> Vec<u8> {
    element(id, &v.to_bits().to_be_bytes())
}

fn string_elem(id: u32, s: &str) -> Vec<u8> {
    element(id, s.as_bytes())
}

fn signed_vint(d: i64) -> Vec<u8> {
    for len in 1..=8usize {
        let bias = (1i64 << (7 * len - 1)) - 1;
        if d.abs() <= bias {
            return (((d + bias) as u64) | (1u64 << (7 * len))).to_be_bytes()[8 - len..].to_vec();
        }
    }
    unreachable!("deltas are small")
}

/// Byte `j` of frame `id` in the fixture pattern.
pub fn mkv_frame_byte(id: u32, j: u32) -> u8 {
    (id.wrapping_mul(29)
        .wrapping_add(j.wrapping_mul(5))
        .wrapping_add(3)
        & 0xFF) as u8
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lacing {
    None,
    Xiph,
    Ebml,
    Fixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MkvGroup {
    pub duration: Option<u64>,
    /// Write a `ReferenceBlock` (the block is not a keyframe).
    pub reference: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MkvBlock {
    /// Track number: 1 = video (`V_MPEG4/ISO/AVC`), 2 = audio (`A_OPUS`).
    pub track: u8,
    /// Timestamp relative to the cluster, in TimestampScale units.
    pub rel_ts: i16,
    /// SimpleBlock keyframe flag (ignored for blocks inside a `BlockGroup`).
    pub key: bool,
    /// Sizes of the frames carried (more than one needs a lacing mode).
    pub frames: Vec<u32>,
    pub lacing: Lacing,
    pub group: Option<MkvGroup>,
    /// Replace the generated lace header (bytes after the flags byte) to build hostile blocks.
    pub raw_lace_header: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MkvCluster {
    pub timecode: u64,
    pub blocks: Vec<MkvBlock>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MkvSpec {
    pub doc_type: &'static str,
    /// TimestampScale in nanoseconds per unit (1_000_000 = milliseconds).
    pub timecode_scale: u64,
    /// Segment and Clusters written with the unknown-size marker.
    pub unknown_sizes: bool,
    pub clusters: Vec<MkvCluster>,
    pub with_cues: bool,
    pub video_default_duration_ns: Option<u64>,
    pub content_encoding_on_video: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MkvFrameInfo {
    /// Global frame id in file order (the key of `mkv_frame_byte`).
    pub id: u32,
    pub track: u8,
    pub cluster: usize,
    pub block: usize,
    pub index_in_block: usize,
    pub size: u32,
    /// Absolute file offset of the frame's first byte.
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mkv {
    pub bytes: Vec<u8>,
    pub frames: Vec<MkvFrameInfo>,
    /// Segment-relative position of each cluster (what `CueClusterPosition` stores).
    pub cluster_positions: Vec<u64>,
    pub segment_data_start: u64,
}

fn tracks_payload(spec: &MkvSpec, (width, height): (u64, u64)) -> Vec<u8> {
    let mut video = uint_elem(0xD7, 1);
    video.extend(uint_elem(0x73C5, 11));
    video.extend(uint_elem(0x83, 1));
    video.extend(uint_elem(0x9C, 0));
    if let Some(ns) = spec.video_default_duration_ns {
        video.extend(uint_elem(0x23E383, ns));
    }
    video.extend(string_elem(0x86, "V_MPEG4/ISO/AVC"));
    video.extend(element(0x63A2, &[1, 2, 3, 4]));
    let mut dims = uint_elem(0xB0, width);
    dims.extend(uint_elem(0xBA, height));
    video.extend(element(0xE0, &dims));
    if spec.content_encoding_on_video {
        video.extend(element(0x6D80, &element(0x6240, &uint_elem(0x5031, 0))));
    }
    let mut audio = uint_elem(0xD7, 2);
    audio.extend(uint_elem(0x73C5, 22));
    audio.extend(uint_elem(0x83, 2));
    audio.extend(uint_elem(0x9C, 1));
    audio.extend(uint_elem(0x23E383, 20_000_000));
    audio.extend(string_elem(0x86, "A_OPUS"));
    audio.extend(uint_elem(0x56AA, 6_500_000));
    audio.extend(uint_elem(0x56BB, 80_000_000));
    let mut params = float_elem(0xB5, 48000.0);
    params.extend(uint_elem(0x9F, 2));
    audio.extend(element(0xE1, &params));
    let mut out = element(0xAE, &video);
    out.extend(element(0xAE, &audio));
    out
}

/// Block payload (track vint, timestamp, flags, lace header) and the offsets of each frame inside it.
fn block_payload(block: &MkvBlock, frame_ids: &mut u32) -> (Vec<u8>, Vec<(usize, u32, u32)>) {
    let mut out = vec![0x80 | block.track];
    out.extend_from_slice(&block.rel_ts.to_be_bytes());
    let simple = block.group.is_none();
    let lacing_bits = match block.lacing {
        Lacing::None => 0,
        Lacing::Xiph => 1,
        Lacing::Fixed => 2,
        Lacing::Ebml => 3,
    };
    out.push((u8::from(simple && block.key) << 7) | (lacing_bits << 1));
    if block.lacing != Lacing::None {
        out.push((block.frames.len() - 1) as u8);
        let n = block.frames.len();
        match (&block.raw_lace_header, block.lacing) {
            (Some(raw), _) => out.extend_from_slice(raw),
            (None, Lacing::Xiph) => {
                for size in &block.frames[..n - 1] {
                    let mut s = *size;
                    while s >= 255 {
                        out.push(255);
                        s -= 255;
                    }
                    out.push(s as u8);
                }
            }
            (None, Lacing::Ebml) => {
                if n >= 2 {
                    out.extend(size_vint(u64::from(block.frames[0])));
                }
                for pair in block.frames[..n - 1].windows(2) {
                    out.extend(signed_vint(i64::from(pair[1]) - i64::from(pair[0])));
                }
            }
            _ => {}
        }
    }
    let mut offsets = Vec::new();
    for size in &block.frames {
        offsets.push((out.len(), *frame_ids, *size));
        out.extend((0..*size).map(|j| mkv_frame_byte(*frame_ids, j)));
        *frame_ids += 1;
    }
    (out, offsets)
}

pub fn mkv_bytes(spec: &MkvSpec) -> Mkv {
    mkv_bytes_with_video_dims(spec, 64, 36)
}

/// `mkv_bytes` with the video track's declared `PixelWidth` / `PixelHeight` chosen by the caller (any
/// 64-bit EBML unsigned value, to forge hostile dimensions).
pub fn mkv_bytes_with_video_dims(spec: &MkvSpec, width: u64, height: u64) -> Mkv {
    let mut file = element(0x1A45DFA3, &{
        let mut h = uint_elem(0x4286, 1);
        h.extend(uint_elem(0x42F7, 1));
        h.extend(uint_elem(0x42F2, 4));
        h.extend(uint_elem(0x42F3, 8));
        h.extend(string_elem(0x4282, spec.doc_type));
        h.extend(uint_elem(0x4287, 4));
        h.extend(uint_elem(0x4285, 2));
        h
    });

    // Segment body: Void, Info, Tracks, Clusters, Cues. Positions are relative to the body start.
    let mut body = element(0xEC, &[0; 10]);
    let last_tc = spec.clusters.last().map_or(0, |c| c.timecode) + 200;
    let mut info = uint_elem(0x2AD7B1, spec.timecode_scale);
    info.extend(float_elem(0x4489, last_tc as f64));
    info.extend(string_elem(0x4D80, "hsk-studio-reel-fixture"));
    body.extend(element(0x1549A966, &info));
    body.extend(element(0x1654AE6B, &tracks_payload(spec, (width, height))));

    let mut frame_ids = 0u32;
    let mut cluster_positions = Vec::new();
    // (time, cluster index, block position inside the cluster payload)
    let mut cue_points: Vec<(u64, usize, u64)> = Vec::new();
    // Frames with body-relative offsets; made absolute once the Segment header length is known.
    let mut pending: Vec<(MkvFrameInfo, u64)> = Vec::new();
    for (ci, cluster) in spec.clusters.iter().enumerate() {
        let mut payload = uint_elem(0xE7, cluster.timecode);
        let mut frame_offsets: Vec<(MkvFrameInfo, usize)> = Vec::new();
        for (bi, block) in cluster.blocks.iter().enumerate() {
            let (data, offsets) = block_payload(block, &mut frame_ids);
            let (element_bytes, data_at) = match block.group {
                Some(g) => {
                    let mut inner = element(0xA1, &data);
                    if let Some(d) = g.duration {
                        inner.extend(uint_elem(0x9B, d));
                    }
                    if g.reference {
                        inner.extend(element(0xFB, &[0x7F]));
                    }
                    let group = element(0xA0, &inner);
                    let group_header = group.len() - inner.len();
                    let block_header = id_bytes(0xA1).len() + size_vint(data.len() as u64).len();
                    (group, group_header + block_header)
                }
                None => {
                    let e = element(0xA3, &data);
                    let at = e.len() - data.len();
                    (e, at)
                }
            };
            if block.track == 1 && block.key && block.group.is_none() {
                cue_points.push((
                    cluster
                        .timecode
                        .wrapping_add_signed(i64::from(block.rel_ts)),
                    ci,
                    payload.len() as u64,
                ));
            }
            for (index_in_block, (at, fid, size)) in offsets.iter().enumerate() {
                frame_offsets.push((
                    MkvFrameInfo {
                        id: *fid,
                        track: block.track,
                        cluster: ci,
                        block: bi,
                        index_in_block,
                        size: *size,
                        offset: 0,
                    },
                    payload.len() + data_at + at,
                ));
            }
            payload.extend(element_bytes);
        }
        let position = body.len() as u64;
        cluster_positions.push(position);
        let header_len = id_bytes(0x1F43B675).len()
            + if spec.unknown_sizes {
                UNKNOWN_SIZE.len()
            } else {
                size_vint(payload.len() as u64).len()
            };
        for (info, rel) in frame_offsets {
            pending.push((info, position + header_len as u64 + rel as u64));
        }
        if spec.unknown_sizes {
            body.extend(id_bytes(0x1F43B675));
            body.extend_from_slice(&UNKNOWN_SIZE);
            body.extend(payload);
        } else {
            body.extend(element(0x1F43B675, &payload));
        }
    }
    if spec.with_cues {
        let mut cues = Vec::new();
        for (time, ci, block_position) in &cue_points {
            let mut positions = uint_elem(0xF7, 1);
            positions.extend(uint_elem(0xF1, cluster_positions[*ci]));
            positions.extend(uint_elem(0xF0, *block_position));
            let mut point = uint_elem(0xB3, *time);
            point.extend(element(0xB7, &positions));
            cues.extend(element(0xBB, &point));
        }
        body.extend(element(0x1C53BB6B, &cues));
    }

    file.extend(id_bytes(0x18538067));
    if spec.unknown_sizes {
        file.extend_from_slice(&UNKNOWN_SIZE);
    } else {
        file.extend(size_vint(body.len() as u64));
    }
    let segment_data_start = file.len() as u64;
    file.extend(body);
    let frames = pending
        .into_iter()
        .map(|(mut info, rel)| {
            info.offset = segment_data_start + rel;
            info
        })
        .collect();
    Mkv {
        bytes: file,
        frames,
        cluster_positions,
        segment_data_start,
    }
}
