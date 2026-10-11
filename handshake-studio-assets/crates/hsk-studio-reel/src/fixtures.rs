//! Synthetic media byte builders (WAV, Y4M, MP4/MOV skeleton). Used by tests and the consumer example so
//! no media is downloaded or stored. Deterministic: every byte is a pure function of its position.

/// 16-bit sample of `channel` at `frame` (never touches a codec: independent oracle for tests).
pub fn pcm16_sample(frame: u64, channel: u16) -> i16 {
    (((frame * 31 + u64::from(channel) * 7) & 0x7FFF) as i16) - 0x2000
}

/// Interleaved s16le WAV (`WAVE_FORMAT_PCM`).
pub fn wav_pcm16(rate: u32, channels: u16, frames: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity((frames * u64::from(channels) * 2) as usize);
    for f in 0..frames {
        for c in 0..channels {
            data.extend_from_slice(&pcm16_sample(f, c).to_le_bytes());
        }
    }
    wav_with_tag(1, rate, channels, 16, &data)
}

/// RIFF/WAVE container around raw `data` with an arbitrary format tag (to exercise unsupported tags).
pub fn wav_with_tag(tag: u16, rate: u32, channels: u16, bits: u16, data: &[u8]) -> Vec<u8> {
    let block = channels * (bits / 8);
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32 + (data.len() as u32 % 2)).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
    out.extend_from_slice(&block.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// Byte `index` of frame `frame` in the Y4M fixture pattern.
pub fn y4m_byte(frame: u64, index: u64) -> u8 {
    ((frame * 37 + index * 11) & 0xFF) as u8
}

/// Size of one frame for chroma `"420" | "422" | "444" | "mono"`.
pub fn y4m_frame_bytes(width: u32, height: u32, chroma: &str) -> usize {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    match chroma {
        "420" => w * h + 2 * cw * ch,
        "422" => w * h + 2 * cw * h,
        "444" => 3 * w * h,
        _ => w * h,
    }
}

pub fn y4m_frame(frame: u64, width: u32, height: u32, chroma: &str) -> Vec<u8> {
    (0..y4m_frame_bytes(width, height, chroma) as u64)
        .map(|i| y4m_byte(frame, i))
        .collect()
}

pub fn y4m_bytes(width: u32, height: u32, rate: (u32, u32), chroma: &str, frames: u64) -> Vec<u8> {
    let tag = if chroma == "420" { "420jpeg" } else { chroma };
    let mut out = format!(
        "YUV4MPEG2 W{width} H{height} F{}:{} Ip A1:1 C{tag}\n",
        rate.0, rate.1
    )
    .into_bytes();
    for f in 0..frames {
        out.extend_from_slice(b"FRAME\n");
        out.extend_from_slice(&y4m_frame(f, width, height, chroma));
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mp4Sample {
    pub duration: u32,
    /// Composition offset (`ctts`); negative needs `ctts_version == 1`.
    pub cts_offset: i32,
    pub size: u32,
    pub sync: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4Spec {
    pub movie_timescale: u32,
    pub media_timescale: u32,
    pub codec: [u8; 4],
    pub width: u16,
    pub height: u16,
    pub samples: Vec<Mp4Sample>,
    /// `(segment_duration in movie units, media_time in media units; -1 = empty edit)`.
    pub edits: Option<Vec<(u64, i64)>>,
    pub samples_per_chunk: u32,
    pub co64: bool,
    pub ctts_version: u8,
    pub moov_first: bool,
}

/// Byte `offset` of sample `index` in the MP4 `mdat` fixture pattern.
pub fn mp4_sample_byte(index: u32, offset: u32) -> u8 {
    (index
        .wrapping_mul(13)
        .wrapping_add(offset.wrapping_mul(7))
        .wrapping_add(1)
        & 0xFF) as u8
}

fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&(payload.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

fn full_box(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(payload.len() + 4);
    body.extend_from_slice(&(u32::from(version) << 24 | flags).to_be_bytes());
    body.extend_from_slice(payload);
    boxed(kind, &body)
}

fn rle<T: PartialEq + Copy>(values: impl Iterator<Item = T>) -> Vec<(u32, T)> {
    let mut runs: Vec<(u32, T)> = Vec::new();
    for v in values {
        match runs.last_mut() {
            Some((n, last)) if *last == v => *n += 1,
            _ => runs.push((1, v)),
        }
    }
    runs
}

fn moov(spec: &Mp4Spec, mdat_payload_offset: u64) -> Vec<u8> {
    let n = spec.samples.len();
    let be32 = |v: u32| v.to_be_bytes();
    let total_media: u64 = spec.samples.iter().map(|s| u64::from(s.duration)).sum();
    let movie_duration =
        total_media * u64::from(spec.movie_timescale) / u64::from(spec.media_timescale);

    let mut mvhd = Vec::new();
    mvhd.extend_from_slice(&[0; 8]);
    mvhd.extend_from_slice(&be32(spec.movie_timescale));
    mvhd.extend_from_slice(&be32(movie_duration as u32));
    mvhd.extend_from_slice(&be32(0x0001_0000));
    mvhd.extend_from_slice(&0x0100u16.to_be_bytes());
    mvhd.extend_from_slice(&[0; 10]);
    for m in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        mvhd.extend_from_slice(&be32(m));
    }
    mvhd.extend_from_slice(&[0; 24]);
    mvhd.extend_from_slice(&be32(2));

    let mut tkhd = Vec::new();
    tkhd.extend_from_slice(&[0; 8]);
    tkhd.extend_from_slice(&be32(1));
    tkhd.extend_from_slice(&[0; 4]);
    tkhd.extend_from_slice(&be32(movie_duration as u32));
    tkhd.extend_from_slice(&[0; 8 + 2 + 2 + 2 + 2]);
    for m in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        tkhd.extend_from_slice(&be32(m));
    }
    tkhd.extend_from_slice(&be32(u32::from(spec.width) << 16));
    tkhd.extend_from_slice(&be32(u32::from(spec.height) << 16));

    let mut edts = Vec::new();
    if let Some(edits) = &spec.edits {
        let wide = edits
            .iter()
            .any(|(d, m)| *d > u64::from(u32::MAX) || *m > i64::from(i32::MAX));
        let mut elst = Vec::new();
        elst.extend_from_slice(&be32(edits.len() as u32));
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
        edts = boxed(b"edts", &full_box(b"elst", u8::from(wide), 0, &elst));
    }

    let mut mdhd = Vec::new();
    mdhd.extend_from_slice(&[0; 8]);
    mdhd.extend_from_slice(&be32(spec.media_timescale));
    mdhd.extend_from_slice(&be32(total_media as u32));
    mdhd.extend_from_slice(&[0x55, 0xC4, 0, 0]);

    let mut hdlr = Vec::new();
    hdlr.extend_from_slice(&[0; 4]);
    hdlr.extend_from_slice(b"vide");
    hdlr.extend_from_slice(&[0; 12]);
    hdlr.extend_from_slice(b"VideoHandler\0");

    let mut entry = Vec::new();
    entry.extend_from_slice(&[0; 6]);
    entry.extend_from_slice(&1u16.to_be_bytes());
    entry.extend_from_slice(&[0; 16]);
    entry.extend_from_slice(&spec.width.to_be_bytes());
    entry.extend_from_slice(&spec.height.to_be_bytes());
    entry.extend_from_slice(&be32(0x0048_0000));
    entry.extend_from_slice(&be32(0x0048_0000));
    entry.extend_from_slice(&[0; 4]);
    entry.extend_from_slice(&1u16.to_be_bytes());
    entry.extend_from_slice(&[0; 32]);
    entry.extend_from_slice(&0x0018u16.to_be_bytes());
    entry.extend_from_slice(&0xFFFFu16.to_be_bytes());
    let mut stsd = be32(1).to_vec();
    stsd.extend_from_slice(&boxed(&spec.codec, &entry));

    let mut stts = Vec::new();
    let runs = rle(spec.samples.iter().map(|s| s.duration));
    stts.extend_from_slice(&be32(runs.len() as u32));
    for (count, delta) in runs {
        stts.extend_from_slice(&be32(count));
        stts.extend_from_slice(&be32(delta));
    }

    let mut stbl = full_box(b"stsd", 0, 0, &stsd);
    stbl.extend_from_slice(&full_box(b"stts", 0, 0, &stts));
    if spec.samples.iter().any(|s| s.cts_offset != 0) {
        let runs = rle(spec.samples.iter().map(|s| s.cts_offset));
        let mut ctts = be32(runs.len() as u32).to_vec();
        for (count, offset) in runs {
            ctts.extend_from_slice(&be32(count));
            ctts.extend_from_slice(&offset.to_be_bytes());
        }
        stbl.extend_from_slice(&full_box(b"ctts", spec.ctts_version, 0, &ctts));
    }
    if spec.samples.iter().any(|s| !s.sync) {
        let syncs: Vec<u32> = (0..n as u32)
            .filter(|i| spec.samples[*i as usize].sync)
            .map(|i| i + 1)
            .collect();
        let mut stss = be32(syncs.len() as u32).to_vec();
        for s in syncs {
            stss.extend_from_slice(&be32(s));
        }
        stbl.extend_from_slice(&full_box(b"stss", 0, 0, &stss));
    }
    let mut stsz = be32(0).to_vec();
    stsz.extend_from_slice(&be32(n as u32));
    for s in &spec.samples {
        stsz.extend_from_slice(&be32(s.size));
    }
    stbl.extend_from_slice(&full_box(b"stsz", 0, 0, &stsz));

    let per = spec.samples_per_chunk.max(1) as usize;
    let chunks = n.div_ceil(per);
    let mut stsc_entries = vec![(1u32, per as u32)];
    if !n.is_multiple_of(per) {
        stsc_entries.push((chunks as u32, (n % per) as u32));
    }
    let mut stsc = be32(stsc_entries.len() as u32).to_vec();
    for (first, count) in stsc_entries {
        stsc.extend_from_slice(&be32(first));
        stsc.extend_from_slice(&be32(count));
        stsc.extend_from_slice(&be32(1));
    }
    stbl.extend_from_slice(&full_box(b"stsc", 0, 0, &stsc));

    let mut offsets = Vec::new();
    let mut cursor = mdat_payload_offset;
    for (i, s) in spec.samples.iter().enumerate() {
        if i % per == 0 {
            offsets.push(cursor);
        }
        cursor += u64::from(s.size);
    }
    let mut table = be32(offsets.len() as u32).to_vec();
    for o in offsets {
        if spec.co64 {
            table.extend_from_slice(&o.to_be_bytes());
        } else {
            table.extend_from_slice(&be32(o as u32));
        }
    }
    stbl.extend_from_slice(&full_box(
        if spec.co64 { b"co64" } else { b"stco" },
        0,
        0,
        &table,
    ));

    let minf = {
        let mut m = full_box(b"vmhd", 0, 1, &[0; 8]);
        m.extend_from_slice(&boxed(b"stbl", &stbl));
        boxed(b"minf", &m)
    };
    let mut mdia = full_box(b"mdhd", 0, 0, &mdhd);
    mdia.extend_from_slice(&full_box(b"hdlr", 0, 0, &hdlr));
    mdia.extend_from_slice(&minf);
    let mut trak = full_box(b"tkhd", 0, 3, &tkhd);
    trak.extend_from_slice(&edts);
    trak.extend_from_slice(&boxed(b"mdia", &mdia));
    let mut movie = full_box(b"mvhd", 0, 0, &mvhd);
    movie.extend_from_slice(&boxed(b"trak", &trak));
    boxed(b"moov", &movie)
}

/// Progressive MP4/MOV with one video track and a synthetic `mdat`.
pub fn mp4_bytes(spec: &Mp4Spec) -> Vec<u8> {
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"isom");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"isomiso2");
    let ftyp = boxed(b"ftyp", &ftyp);
    let mut mdat_payload = Vec::new();
    for (i, s) in spec.samples.iter().enumerate() {
        mdat_payload.extend((0..s.size).map(|j| mp4_sample_byte(i as u32, j)));
    }
    let mdat = boxed(b"mdat", &mdat_payload);
    let mut out = ftyp.clone();
    if spec.moov_first {
        let moov_len = moov(spec, 0).len() as u64;
        let base = ftyp.len() as u64 + moov_len + 8;
        out.extend_from_slice(&moov(spec, base));
        out.extend_from_slice(&mdat);
    } else {
        let base = ftyp.len() as u64 + 8;
        out.extend_from_slice(&mdat);
        out.extend_from_slice(&moov(spec, base));
    }
    out
}
