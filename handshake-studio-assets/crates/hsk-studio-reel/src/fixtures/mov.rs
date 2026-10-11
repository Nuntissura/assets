//! Synthetic QuickTime/ISOBMFF builders for slice 2: PCM audio MOV, timecode track MOV, fragmented MP4.
//! Independent of the readers: they write boxes from the published layouts, never from reader code.
use super::{Mp4Sample, boxed, full_box, mp4_sample_byte};

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

struct TrakSpec {
    track_id: u32,
    handler: [u8; 4],
    media_timescale: u32,
    media_duration: u64,
    movie_duration: u64,
    width: u16,
    height: u16,
    /// Complete sample entry box (header included).
    entry: Vec<u8>,
    stts: Vec<(u32, u32)>,
    stsz_fixed: u32,
    stsz_count: u32,
    stsz_table: Vec<u32>,
    stsc: Vec<(u32, u32)>,
    chunk_offsets: Vec<u64>,
    edits: Option<Vec<(u64, i64)>>,
    tref_tmcd: Option<u32>,
}

fn elst_box(edits: &[(u64, i64)]) -> Vec<u8> {
    let wide = edits
        .iter()
        .any(|(d, m)| *d > u64::from(u32::MAX) || *m > i64::from(i32::MAX));
    let mut elst = be32(edits.len() as u32).to_vec();
    for (duration, media_time) in edits {
        if wide {
            elst.extend_from_slice(&duration.to_be_bytes());
            elst.extend_from_slice(&media_time.to_be_bytes());
        } else {
            elst.extend_from_slice(&be32(*duration as u32));
            elst.extend_from_slice(&(*media_time as i32).to_be_bytes());
        }
        elst.extend_from_slice(&be32(0x0001_0000));
    }
    boxed(b"edts", &full_box(b"elst", u8::from(wide), 0, &elst))
}

fn matrix() -> Vec<u8> {
    [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000]
        .iter()
        .flat_map(|m| be32(*m))
        .collect()
}

fn trak_box(t: &TrakSpec) -> Vec<u8> {
    let mut tkhd = vec![0u8; 8];
    tkhd.extend_from_slice(&be32(t.track_id));
    tkhd.extend_from_slice(&[0; 4]);
    tkhd.extend_from_slice(&be32(t.movie_duration as u32));
    tkhd.extend_from_slice(&[0; 16]);
    tkhd.extend_from_slice(&matrix());
    tkhd.extend_from_slice(&be32(u32::from(t.width) << 16));
    tkhd.extend_from_slice(&be32(u32::from(t.height) << 16));

    let mut mdhd = vec![0u8; 8];
    mdhd.extend_from_slice(&be32(t.media_timescale));
    mdhd.extend_from_slice(&be32(t.media_duration as u32));
    mdhd.extend_from_slice(&[0x55, 0xC4, 0, 0]);

    let mut hdlr = vec![0u8; 4];
    hdlr.extend_from_slice(&t.handler);
    hdlr.extend_from_slice(&[0; 12]);
    hdlr.extend_from_slice(b"Handler\0");

    let mut stsd = be32(1).to_vec();
    stsd.extend_from_slice(&t.entry);
    let mut stts = be32(t.stts.len() as u32).to_vec();
    for (count, delta) in &t.stts {
        stts.extend_from_slice(&be32(*count));
        stts.extend_from_slice(&be32(*delta));
    }
    let mut stsz = be32(t.stsz_fixed).to_vec();
    stsz.extend_from_slice(&be32(t.stsz_count));
    for size in &t.stsz_table {
        stsz.extend_from_slice(&be32(*size));
    }
    let mut stsc = be32(t.stsc.len() as u32).to_vec();
    for (first, count) in &t.stsc {
        stsc.extend_from_slice(&be32(*first));
        stsc.extend_from_slice(&be32(*count));
        stsc.extend_from_slice(&be32(1));
    }
    let mut stco = be32(t.chunk_offsets.len() as u32).to_vec();
    for offset in &t.chunk_offsets {
        stco.extend_from_slice(&be32(*offset as u32));
    }
    let mut stbl = full_box(b"stsd", 0, 0, &stsd);
    stbl.extend_from_slice(&full_box(b"stts", 0, 0, &stts));
    stbl.extend_from_slice(&full_box(b"stsz", 0, 0, &stsz));
    stbl.extend_from_slice(&full_box(b"stsc", 0, 0, &stsc));
    stbl.extend_from_slice(&full_box(b"stco", 0, 0, &stco));

    let mut minf = match &t.handler {
        b"vide" => full_box(b"vmhd", 0, 1, &[0; 8]),
        b"soun" => full_box(b"smhd", 0, 0, &[0; 4]),
        _ => full_box(b"nmhd", 0, 0, &[]),
    };
    minf.extend_from_slice(&boxed(b"stbl", &stbl));
    let mut mdia = full_box(b"mdhd", 0, 0, &mdhd);
    mdia.extend_from_slice(&full_box(b"hdlr", 0, 0, &hdlr));
    mdia.extend_from_slice(&boxed(b"minf", &minf));

    let mut trak = full_box(b"tkhd", 0, 3, &tkhd);
    if let Some(tc) = t.tref_tmcd {
        trak.extend_from_slice(&boxed(b"tref", &boxed(b"tmcd", &be32(tc))));
    }
    if let Some(edits) = &t.edits {
        trak.extend_from_slice(&elst_box(edits));
    }
    trak.extend_from_slice(&boxed(b"mdia", &mdia));
    boxed(b"trak", &trak)
}

fn mvhd_box(movie_timescale: u32, movie_duration: u64) -> Vec<u8> {
    let mut mvhd = vec![0u8; 8];
    mvhd.extend_from_slice(&be32(movie_timescale));
    mvhd.extend_from_slice(&be32(movie_duration as u32));
    mvhd.extend_from_slice(&be32(0x0001_0000));
    mvhd.extend_from_slice(&0x0100u16.to_be_bytes());
    mvhd.extend_from_slice(&[0; 10]);
    mvhd.extend_from_slice(&matrix());
    mvhd.extend_from_slice(&[0; 24]);
    mvhd.extend_from_slice(&be32(9));
    full_box(b"mvhd", 0, 0, &mvhd)
}

fn ftyp_box(brand: &[u8; 4]) -> Vec<u8> {
    let mut ftyp = brand.to_vec();
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(brand);
    boxed(b"ftyp", &ftyp)
}

fn video_entry(codec: &[u8; 4], width: u16, height: u16) -> Vec<u8> {
    let mut entry = vec![0u8; 6];
    entry.extend_from_slice(&1u16.to_be_bytes());
    entry.extend_from_slice(&[0; 16]);
    entry.extend_from_slice(&width.to_be_bytes());
    entry.extend_from_slice(&height.to_be_bytes());
    entry.extend_from_slice(&be32(0x0048_0000));
    entry.extend_from_slice(&be32(0x0048_0000));
    entry.extend_from_slice(&[0; 4]);
    entry.extend_from_slice(&1u16.to_be_bytes());
    entry.extend_from_slice(&[0; 32]);
    entry.extend_from_slice(&0x0018u16.to_be_bytes());
    entry.extend_from_slice(&0xFFFFu16.to_be_bytes());
    boxed(codec, &entry)
}

/// ftyp + moov (offset-dependent, built twice) + mdat in either order.
fn assemble(moov: &dyn Fn(u64) -> Vec<u8>, mdat_payload: &[u8], moov_first: bool) -> Vec<u8> {
    let ftyp = ftyp_box(b"qt  ");
    let mdat = boxed(b"mdat", mdat_payload);
    let mut out = ftyp.clone();
    if moov_first {
        let base = ftyp.len() as u64 + moov(0).len() as u64 + 8;
        out.extend_from_slice(&moov(base));
        out.extend_from_slice(&mdat);
    } else {
        out.extend_from_slice(&mdat);
        out.extend_from_slice(&moov(ftyp.len() as u64 + 8));
    }
    out
}

// ---- PCM audio in MOV ------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcmKind {
    S8,
    U8,
    S16,
    S24,
    S32,
    F32,
    F64,
}

impl PcmKind {
    pub fn bits(self) -> u16 {
        match self {
            Self::S8 | Self::U8 => 8,
            Self::S16 => 16,
            Self::S24 => 24,
            Self::S32 | Self::F32 => 32,
            Self::F64 => 64,
        }
    }
    pub fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }
}

/// Underlying 16-bit-range value of `channel` at `frame`, shared by every PCM kind.
pub fn pcm_sample_value(frame: u64, channel: u16) -> i32 {
    ((frame * 31 + u64::from(channel) * 7) & 0x7FFF) as i32 - 0x2000
}

/// Encode `value` as `kind` in the requested byte order. The oracle for expected little-endian output.
pub fn pcm_encode(kind: PcmKind, big_endian: bool, value: i32) -> Vec<u8> {
    let mut bytes = match kind {
        PcmKind::S8 => vec![(value >> 8) as i8 as u8],
        PcmKind::U8 => vec![((value >> 8) + 128) as u8],
        PcmKind::S16 => (value as i16).to_be_bytes().to_vec(),
        PcmKind::S24 => ((value << 8).to_be_bytes()[1..4]).to_vec(),
        PcmKind::S32 => (value << 16).to_be_bytes().to_vec(),
        PcmKind::F32 => (value as f32 / 32768.0).to_bits().to_be_bytes().to_vec(),
        PcmKind::F64 => (f64::from(value) / 32768.0)
            .to_bits()
            .to_be_bytes()
            .to_vec(),
    };
    if !big_endian {
        bytes.reverse();
    }
    bytes
}

#[derive(Debug, Clone, PartialEq)]
pub struct PcmMovSpec {
    /// Sample-entry FourCC (`twos`, `sowt`, `in24`, `in32`, `fl32`, `fl64`, `raw `, `lpcm`).
    pub fourcc: [u8; 4],
    pub kind: PcmKind,
    /// Byte order of the stored samples (`lpcm` carries it in its flags).
    pub big_endian: bool,
    /// Write a version 2 sound description (required for `lpcm`).
    pub lpcm_v2: bool,
    /// `lpcm` only: write the non-interleaved flag (must be rejected by readers).
    pub non_interleaved: bool,
    pub channels: u16,
    pub rate: u32,
    pub frames: u64,
    pub frames_per_chunk: u32,
    pub movie_timescale: u32,
    pub edits: Option<Vec<(u64, i64)>>,
    pub moov_first: bool,
}

pub fn mov_pcm_bytes(spec: &PcmMovSpec) -> Vec<u8> {
    let block = u64::from(spec.channels) * u64::from(spec.kind.bits() / 8);
    let mut payload = Vec::with_capacity((spec.frames * block) as usize);
    for f in 0..spec.frames {
        for c in 0..spec.channels {
            payload.extend(pcm_encode(
                spec.kind,
                spec.big_endian,
                pcm_sample_value(f, c),
            ));
        }
    }
    let mut body = vec![0u8; 6];
    body.extend_from_slice(&1u16.to_be_bytes());
    if spec.lpcm_v2 {
        let mut flags = 8u32; // packed
        flags |= u32::from(spec.kind.is_float());
        flags |= u32::from(spec.big_endian) << 1;
        flags |= u32::from(!spec.kind.is_float() && spec.kind != PcmKind::U8) << 2;
        flags |= u32::from(spec.non_interleaved) << 5;
        body.extend_from_slice(&2u16.to_be_bytes());
        body.extend_from_slice(&[0; 6]);
        body.extend_from_slice(&3u16.to_be_bytes());
        body.extend_from_slice(&16u16.to_be_bytes());
        body.extend_from_slice(&0xFFFEu16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&be32(65536));
        body.extend_from_slice(&be32(72));
        body.extend_from_slice(&f64::from(spec.rate).to_bits().to_be_bytes());
        body.extend_from_slice(&be32(u32::from(spec.channels)));
        body.extend_from_slice(&be32(0x7F00_0000));
        body.extend_from_slice(&be32(u32::from(spec.kind.bits())));
        body.extend_from_slice(&be32(flags));
        body.extend_from_slice(&be32(block as u32));
        body.extend_from_slice(&be32(1));
    } else {
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&[0; 6]);
        body.extend_from_slice(&spec.channels.to_be_bytes());
        body.extend_from_slice(&spec.kind.bits().to_be_bytes());
        body.extend_from_slice(&[0; 4]);
        body.extend_from_slice(&be32(spec.rate << 16));
    }
    let entry = boxed(&spec.fourcc, &body);
    let per = u64::from(spec.frames_per_chunk.max(1)).min(spec.frames.max(1));
    let chunks = spec.frames.div_ceil(per);
    let mut stsc = vec![(1u32, per as u32)];
    if !spec.frames.is_multiple_of(per) {
        stsc.push((chunks as u32, (spec.frames % per) as u32));
    }
    let movie_duration = spec.frames * u64::from(spec.movie_timescale) / u64::from(spec.rate);
    let moov = |base: u64| {
        let trak = trak_box(&TrakSpec {
            track_id: 1,
            handler: *b"soun",
            media_timescale: spec.rate,
            media_duration: spec.frames,
            movie_duration,
            width: 0,
            height: 0,
            entry: entry.clone(),
            stts: vec![(spec.frames as u32, 1)],
            stsz_fixed: block as u32,
            stsz_count: spec.frames as u32,
            stsz_table: Vec::new(),
            stsc: stsc.clone(),
            chunk_offsets: (0..chunks).map(|k| base + k * per * block).collect(),
            edits: spec.edits.clone(),
            tref_tmcd: None,
        });
        let mut moov = mvhd_box(spec.movie_timescale, movie_duration);
        moov.extend_from_slice(&trak);
        boxed(b"moov", &moov)
    };
    assemble(&moov, &payload, spec.moov_first)
}

// ---- timecode track --------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimecodeMovSpec {
    pub frames: u32,
    pub timescale: u32,
    pub frame_duration: u32,
    pub frame_bytes: u32,
    pub drop_frame: bool,
    pub counter: bool,
    pub frames_per_second: u8,
    /// The 32-bit timecode sample.
    pub start_value: u32,
}

/// Video track (id 1, `avc1`, all sync) referencing a `tmcd` track (id 2) through `tref`.
pub fn mov_timecode_bytes(spec: &TimecodeMovSpec) -> Vec<u8> {
    let video_bytes = u64::from(spec.frames) * u64::from(spec.frame_bytes);
    let mut payload: Vec<u8> = (0..spec.frames)
        .flat_map(|i| (0..spec.frame_bytes).map(move |j| mp4_sample_byte(i, j)))
        .collect();
    payload.extend_from_slice(&spec.start_value.to_be_bytes());
    let total = u64::from(spec.frames) * u64::from(spec.frame_duration);
    let mut tmcd = vec![0u8; 6];
    tmcd.extend_from_slice(&1u16.to_be_bytes());
    tmcd.extend_from_slice(&[0; 4]);
    let flags = u32::from(spec.drop_frame) | u32::from(spec.counter) << 3;
    tmcd.extend_from_slice(&be32(flags));
    tmcd.extend_from_slice(&be32(spec.timescale));
    tmcd.extend_from_slice(&be32(spec.frame_duration));
    tmcd.extend_from_slice(&[spec.frames_per_second, 0]);
    let tmcd = boxed(b"tmcd", &tmcd);
    let moov = |base: u64| {
        let video = trak_box(&TrakSpec {
            track_id: 1,
            handler: *b"vide",
            media_timescale: spec.timescale,
            media_duration: total,
            movie_duration: total,
            width: 64,
            height: 36,
            entry: video_entry(b"avc1", 64, 36),
            stts: vec![(spec.frames, spec.frame_duration)],
            stsz_fixed: spec.frame_bytes,
            stsz_count: spec.frames,
            stsz_table: Vec::new(),
            stsc: vec![(1, spec.frames)],
            chunk_offsets: vec![base],
            edits: None,
            tref_tmcd: Some(2),
        });
        let timecode = trak_box(&TrakSpec {
            track_id: 2,
            handler: *b"tmcd",
            media_timescale: spec.timescale,
            media_duration: total,
            movie_duration: total,
            width: 0,
            height: 0,
            entry: tmcd.clone(),
            stts: vec![(1, total as u32)],
            stsz_fixed: 4,
            stsz_count: 1,
            stsz_table: Vec::new(),
            stsc: vec![(1, 1)],
            chunk_offsets: vec![base + video_bytes],
            edits: None,
            tref_tmcd: None,
        });
        let mut moov = mvhd_box(spec.timescale, total);
        moov.extend_from_slice(&video);
        moov.extend_from_slice(&timecode);
        boxed(b"moov", &moov)
    };
    assemble(&moov, &payload, true)
}

// ---- fragmented MP4 --------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragSpec {
    pub movie_timescale: u32,
    pub media_timescale: u32,
    pub codec: [u8; 4],
    pub width: u16,
    pub height: u16,
    /// Even fragments carry full per-sample trun fields; odd fragments use tfhd defaults (uniform
    /// duration and size, first sample sync, the rest non-sync by the `trex` default).
    pub fragments: Vec<Vec<Mp4Sample>>,
    /// Include `tfdt` for fragment `i`; otherwise decode time continues from the previous fragment.
    pub tfdt: Vec<bool>,
    pub default_base_is_moof: bool,
    pub with_sidx: bool,
    pub edits: Option<Vec<(u64, i64)>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fmp4 {
    pub bytes: Vec<u8>,
    /// Absolute offset of every `moof` (independent oracle for the scanner).
    pub moof_offsets: Vec<u64>,
    /// Absolute offset each `sidx` reference should point to (empty without `with_sidx`).
    pub sidx_entry_offsets: Vec<u64>,
}

const NON_SYNC: u32 = 0x0001_0000;

fn moof_and_mdat(
    sequence: u32,
    samples: &[Mp4Sample],
    first_global: u32,
    tfdt: Option<u64>,
    style_a: bool,
    default_base_is_moof: bool,
) -> Vec<u8> {
    let build = |data_offset: i32| {
        let mut tfhd_flags = if default_base_is_moof { 0x2_0000 } else { 0 };
        let mut tfhd = be32(1).to_vec();
        if !style_a {
            tfhd_flags |= 0x08 | 0x10;
            tfhd.extend_from_slice(&be32(samples[0].duration));
            tfhd.extend_from_slice(&be32(samples[0].size));
        }
        let mut traf = full_box(b"tfhd", 0, tfhd_flags, &tfhd);
        if let Some(t) = tfdt {
            traf.extend_from_slice(&full_box(b"tfdt", 1, 0, &t.to_be_bytes()));
        }
        let mut trun = be32(samples.len() as u32).to_vec();
        trun.extend_from_slice(&data_offset.to_be_bytes());
        let flags = if style_a {
            for s in samples {
                trun.extend_from_slice(&be32(s.duration));
                trun.extend_from_slice(&be32(s.size));
                trun.extend_from_slice(&be32(if s.sync { 0 } else { NON_SYNC }));
                trun.extend_from_slice(&s.cts_offset.to_be_bytes());
            }
            0x1 | 0x100 | 0x200 | 0x400 | 0x800
        } else {
            trun.extend_from_slice(&be32(0)); // first_sample_flags: sync
            for s in samples {
                trun.extend_from_slice(&s.cts_offset.to_be_bytes());
            }
            0x1 | 0x4 | 0x800
        };
        traf.extend_from_slice(&full_box(b"trun", 1, flags, &trun));
        let mut moof = full_box(b"mfhd", 0, 0, &be32(sequence));
        moof.extend_from_slice(&boxed(b"traf", &traf));
        boxed(b"moof", &moof)
    };
    if !style_a {
        assert!(
            samples
                .iter()
                .all(|s| s.duration == samples[0].duration && s.size == samples[0].size)
                && samples[0].sync
                && samples[1..].iter().all(|s| !s.sync),
            "default-style fragment needs uniform samples, sync first, non-sync rest"
        );
    }
    let moof_len = build(0).len();
    let mut out = build(moof_len as i32 + 8);
    let payload: Vec<u8> = samples
        .iter()
        .enumerate()
        .flat_map(|(i, s)| (0..s.size).map(move |j| mp4_sample_byte(first_global + i as u32, j)))
        .collect();
    out.extend_from_slice(&boxed(b"mdat", &payload));
    out
}

pub fn fmp4_bytes(spec: &FragSpec) -> Fmp4 {
    let trak = trak_box(&TrakSpec {
        track_id: 1,
        handler: *b"vide",
        media_timescale: spec.media_timescale,
        media_duration: 0,
        movie_duration: 0,
        width: spec.width,
        height: spec.height,
        entry: video_entry(&spec.codec, spec.width, spec.height),
        stts: Vec::new(),
        stsz_fixed: 0,
        stsz_count: 0,
        stsz_table: Vec::new(),
        stsc: Vec::new(),
        chunk_offsets: Vec::new(),
        edits: spec.edits.clone(),
        tref_tmcd: None,
    });
    let mut trex = be32(1).to_vec();
    for v in [1, 0, 0, NON_SYNC] {
        trex.extend_from_slice(&be32(v));
    }
    let mut moov = mvhd_box(spec.movie_timescale, 0);
    moov.extend_from_slice(&trak);
    moov.extend_from_slice(&boxed(b"mvex", &full_box(b"trex", 0, 0, &trex)));
    let moov = boxed(b"moov", &moov);

    let mut fragments = Vec::new();
    let (mut dts, mut global) = (0u64, 0u32);
    for (i, samples) in spec.fragments.iter().enumerate() {
        let with_tfdt = spec.tfdt.get(i).copied().unwrap_or(true);
        fragments.push((
            moof_and_mdat(
                i as u32 + 1,
                samples,
                global,
                with_tfdt.then_some(dts),
                i % 2 == 0,
                spec.default_base_is_moof,
            ),
            samples.iter().map(|s| u64::from(s.duration)).sum::<u64>(),
        ));
        dts += fragments[i].1;
        global += samples.len() as u32;
    }
    let ftyp = ftyp_box(b"iso5");
    let sidx = spec.with_sidx.then(|| {
        let mut p = be32(1).to_vec();
        p.extend_from_slice(&be32(spec.media_timescale));
        p.extend_from_slice(&be32(0));
        p.extend_from_slice(&be32(0));
        p.extend_from_slice(&0u16.to_be_bytes());
        p.extend_from_slice(&(fragments.len() as u16).to_be_bytes());
        for (bytes, duration) in &fragments {
            p.extend_from_slice(&be32(bytes.len() as u32));
            p.extend_from_slice(&be32(*duration as u32));
            p.extend_from_slice(&be32(0x9000_0000));
        }
        full_box(b"sidx", 0, 0, &p)
    });
    let mut bytes = ftyp;
    bytes.extend_from_slice(&moov);
    if let Some(sidx) = &sidx {
        bytes.extend_from_slice(sidx);
    }
    let (mut moof_offsets, mut sidx_entry_offsets) = (Vec::new(), Vec::new());
    for (fragment, _) in &fragments {
        moof_offsets.push(bytes.len() as u64);
        if sidx.is_some() {
            sidx_entry_offsets.push(bytes.len() as u64);
        }
        bytes.extend_from_slice(fragment);
    }
    Fmp4 {
        bytes,
        moof_offsets,
        sidx_entry_offsets,
    }
}
