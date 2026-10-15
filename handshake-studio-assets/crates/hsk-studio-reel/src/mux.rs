//! ISO base media file writer (lane SMB-REEL-ENCODE slice 1): MP4 (ISO/IEC 14496-12/14) and QuickTime
//! MOV, written clean-room from the box layouts of ISO/IEC 14496-12 (ftyp, mdat, moov, mvhd, trak, tkhd,
//! edts/elst, mdia, mdhd, hdlr, minf, vmhd/smhd, dinf/dref, stbl, stsd, stts, ctts, stss, stsz, stsc,
//! stco/co64), ISO/IEC 14496-14 (MP4 brands), the AV1-ISOBMFF binding (`av01` + `av1C`), ISO/IEC 23003-5
//! (`ipcm`/`fpcm` + `pcmC`, MP4 PCM) and the QuickTime File Format (version 2 `lpcm` sound description,
//! VC-3 `AVdn`/`AVdh` sample entries, `nclc` colour). No source of another muxer was read.
//!
//! Containers and the combinations each carries (STU-VID-062: an unsupported combination is refused with a
//! determinate result in [`Muxer::new`], before a single sample is encoded or written):
//!
//! | container | video sample entries | audio |
//! |---|---|---|
//! | `Mp4` | `av01`, `avc1` (remux only: reel encodes no H.264, D24) | `ipcm` (16/24/32-bit integer), `fpcm` (32-bit float); rate <= 65535 |
//! | `Mov` | `av01`, `avc1` (remux only), `AVdn` (VC-3 DNxHD), `AVdh` (VC-3 DNxHR) | `lpcm` v2 (16/24/32-bit integer, 32-bit float), little-endian interleaved |
//!
//! Layout: `ftyp`, an 8-byte `wide` placeholder, `mdat` (streamed), `moov` last. When the media data grows
//! past 4 GiB the placeholder and the 32-bit `mdat` header are rewritten in place as one 64-bit `mdat`
//! header, and chunk offsets switch to `co64` when one exceeds 32 bits. Timing is exact: the video media
//! timescale is `254016000000 / gcd(254016000000, ticks_per_frame)` and the sample delta the matching
//! quotient (STU-IO-142), refused when either does not fit 32 bits; the audio media timescale is the
//! sample rate with one sample per PCM frame. Video samples arrive in decode order with their
//! presentation index; a reordered stream gets `ctts` (version 0) plus a one-entry `elst` that removes the
//! decode delay, so presentation starts at zero.
//!
//! Every size, count and offset is bounded by [`MuxLimits`] and checked before it is buffered (V4-01/02).
#![deny(clippy::arithmetic_side_effects)]

use crate::error::ReelError;
use hsk_studio_accord::TICKS_PER_SECOND;
use std::io::{Seek, SeekFrom, Write};

/// Muxer identity copied into export receipts.
pub const MUXER_ID: &str = "reel-isobmff-mux";
pub const MUXER_VERSION: &str = "1";

/// Largest codec configuration record accepted (`av1C`/`avcC`).
pub const MAX_CONFIG_BYTES: usize = 64 * 1024;

fn unsupported(what: &'static str) -> ReelError {
    ReelError::Unsupported { what }
}

fn limit(what: &'static str) -> ReelError {
    ReelError::LimitExceeded { what }
}

/// Output container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// ISO base media / MP4 (`.mp4`).
    Mp4,
    /// QuickTime (`.mov`).
    Mov,
}

impl Container {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
        }
    }

    pub const fn extension(self) -> &'static str {
        self.as_str()
    }
}

/// Video sample entry and its configuration record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoEntry {
    /// AV1: `av01` + `av1C` (validated with `parse_av1c`).
    Av1 { av1c: Vec<u8> },
    /// H.264: `avc1` + `avcC` (validated with `parse_avcc`; SPS and PPS required). Reel itself never encodes
    /// H.264 (Operator decisions D24 / D24-A): the entry re-wraps existing streams or carries the OS encoder's
    /// output (export recipes `VideoCodecChoice::H264`, reel-native `os-encode`).
    H264Remux { avcc: Vec<u8> },
    /// HEVC from the OS encoder (D24-A): `hvc1` + `hvcC` (checked with `os_encode::parse_hvcc`).
    Hevc { hvcc: Vec<u8> },
    /// VC-3: `AVdh` when `dnxhr`, else `AVdn` (QuickTime only).
    Vc3 { dnxhr: bool },
}

impl VideoEntry {
    pub const fn fourcc(&self) -> [u8; 4] {
        match self {
            Self::Av1 { .. } => *b"av01",
            Self::H264Remux { .. } => *b"avc1",
            Self::Hevc { .. } => *b"hvc1",
            Self::Vc3 { dnxhr: true } => *b"AVdh",
            Self::Vc3 { dnxhr: false } => *b"AVdn",
        }
    }

    pub const fn codec(&self) -> &'static str {
        match self {
            Self::Av1 { .. } => "av1",
            Self::H264Remux { .. } => "h264",
            Self::Hevc { .. } => "hevc",
            Self::Vc3 { .. } => "vc3",
        }
    }
}

/// ITU-T H.273 colour code points written as `colr` (`nclx` in MP4, `nclc` in MOV, which has no range
/// flag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NclxColor {
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
    pub full_range: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoTrackSpec {
    pub entry: VideoEntry,
    pub width: u32,
    pub height: u32,
    /// Frame duration in 254016000000-per-second ticks (STU-IO-142).
    pub ticks_per_frame: u64,
    pub color: Option<NclxColor>,
}

/// Interleaved little-endian PCM as handed to [`Muxer::write_audio`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmSpec {
    /// 16, 24 or 32 (integer), 32 (float).
    pub bits: u16,
    pub float: bool,
    pub rate: u32,
    pub channels: u16,
}

impl PcmSpec {
    /// Bytes of one interleaved frame (all channels).
    pub fn block_bytes(&self) -> Option<u32> {
        u32::from(self.channels).checked_mul(u32::from(self.bits / 8))
    }

    /// Sample format name as reel's PCM decoders deliver it.
    pub const fn output_name(&self) -> &'static str {
        match (self.float, self.bits) {
            (true, _) => "pcm_f32le",
            (false, 16) => "pcm_s16le",
            (false, 24) => "pcm_s24le",
            _ => "pcm_s32le",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxSpec {
    pub container: Container,
    pub video: Option<VideoTrackSpec>,
    pub audio: Option<PcmSpec>,
}

/// Resource bounds, checked before anything is buffered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuxLimits {
    /// Video samples (frames) per file.
    pub max_video_samples: usize,
    /// Audio chunks (one per `write_audio` run) per file.
    pub max_audio_chunks: usize,
    /// Largest single video sample.
    pub max_sample_bytes: usize,
    /// Largest picture side.
    pub max_side: u32,
    pub max_channels: u16,
    pub max_rate: u32,
    /// Largest file (media data plus boxes).
    pub max_file_bytes: u64,
}

impl Default for MuxLimits {
    fn default() -> Self {
        Self {
            max_video_samples: 4_000_000,
            max_audio_chunks: 4_000_000,
            max_sample_bytes: 64 * 1024 * 1024,
            max_side: 16384,
            max_channels: 32,
            max_rate: 768_000,
            max_file_bytes: 1 << 40,
        }
    }
}

/// What a finished file holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxReceipt {
    pub container: Container,
    pub muxer_id: &'static str,
    pub muxer_version: &'static str,
    pub video_codec: Option<&'static str>,
    pub video_samples: u64,
    pub video_timescale: u32,
    pub video_sample_delta: u32,
    /// Frames of decode delay removed by the edit list (0 without reordering).
    pub reorder_delay: u64,
    pub audio_format: Option<&'static str>,
    pub audio_frames: u64,
    pub mdat_payload_bytes: u64,
    pub file_bytes: u64,
    pub co64: bool,
    pub large_mdat: bool,
}

/// Brands and the combination check: one place decides what each container carries.
fn check_combination(spec: &MuxSpec, limits: &MuxLimits) -> Result<(), ReelError> {
    if spec.video.is_none() && spec.audio.is_none() {
        return Err(unsupported("mux-no-tracks"));
    }
    if let Some(v) = &spec.video {
        if v.width == 0 || v.height == 0 {
            return Err(unsupported("mux-zero-dimension"));
        }
        if v.width > limits.max_side || v.height > limits.max_side || v.width > u32::from(u16::MAX) {
            return Err(limit("mux-dimensions"));
        }
        if v.ticks_per_frame == 0 {
            return Err(unsupported("mux-zero-frame-duration"));
        }
        match &v.entry {
            VideoEntry::Av1 { av1c } => {
                if av1c.len() > MAX_CONFIG_BYTES {
                    return Err(limit("mux-config-bytes"));
                }
                crate::av1_config::parse_av1c(av1c)?;
            }
            VideoEntry::H264Remux { avcc } => {
                if avcc.len() > MAX_CONFIG_BYTES {
                    return Err(limit("mux-config-bytes"));
                }
                let config = crate::h264_config::parse_avcc(avcc)?;
                if config.sps.is_empty() || config.pps.is_empty() {
                    return Err(unsupported("mux-avc1-needs-parameter-sets"));
                }
            }
            VideoEntry::Hevc { hvcc } => {
                if hvcc.len() > MAX_CONFIG_BYTES {
                    return Err(limit("mux-config-bytes"));
                }
                crate::os_encode::parse_hvcc(hvcc)?;
            }
            VideoEntry::Vc3 { .. } => {
                if spec.container == Container::Mp4 {
                    return Err(unsupported("mux-mp4-cannot-carry-vc3"));
                }
            }
        }
    }
    if let Some(a) = &spec.audio {
        let bits_ok = if a.float {
            a.bits == 32
        } else {
            matches!(a.bits, 16 | 24 | 32)
        };
        if !bits_ok {
            return Err(unsupported("mux-pcm-bit-depth"));
        }
        if a.channels == 0 || a.rate == 0 {
            return Err(unsupported("mux-pcm-zero-channels-or-rate"));
        }
        if a.channels > limits.max_channels || a.rate > limits.max_rate {
            return Err(limit("mux-pcm-layout"));
        }
        if spec.container == Container::Mp4 && a.rate > u32::from(u16::MAX) {
            return Err(unsupported("mux-mp4-pcm-rate-above-65535"));
        }
    }
    Ok(())
}

const fn gcd(mut a: u64, mut b: u64) -> u64 {
    while let Some(r) = a.checked_rem(b) {
        a = b;
        b = r;
    }
    a
}

/// `(timescale, delta)` of a video track whose frames last `ticks_per_frame` ticks.
pub fn video_timescale(ticks_per_frame: u64) -> Result<(u32, u32), ReelError> {
    if ticks_per_frame == 0 {
        return Err(unsupported("mux-zero-frame-duration"));
    }
    let g = gcd(TICKS_PER_SECOND, ticks_per_frame);
    let timescale = TICKS_PER_SECOND.checked_div(g).ok_or(limit("mux-timescale"))?;
    let delta = ticks_per_frame.checked_div(g).ok_or(limit("mux-timescale"))?;
    match (u32::try_from(timescale), u32::try_from(delta)) {
        (Ok(t), Ok(d)) => Ok((t, d)),
        _ => Err(unsupported("mux-frame-rate-not-representable")),
    }
}

#[derive(Debug, Clone, Copy)]
struct Chunk {
    offset: u64,
    samples: u32,
}

#[derive(Debug, Default)]
struct VideoTable {
    sizes: Vec<u32>,
    /// Presentation index of each sample, in decode order.
    pts: Vec<u64>,
    /// 1-based numbers of sync samples.
    sync: Vec<u32>,
    chunks: Vec<Chunk>,
}

#[derive(Debug, Default)]
struct AudioTable {
    frames: u64,
    chunks: Vec<Chunk>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Last {
    None,
    Video,
    Audio,
}

/// Streaming writer: samples go straight to `out`; only the sample tables are kept.
pub struct Muxer<W: Write + Seek> {
    out: W,
    spec: MuxSpec,
    limits: MuxLimits,
    /// File offset of the 8-byte `wide` placeholder in front of `mdat`.
    wide_at: u64,
    /// Next write position (end of media data).
    pos: u64,
    video: VideoTable,
    audio: AudioTable,
    last: Last,
    video_ts: (u32, u32),
}

fn io_err(error: std::io::Error) -> ReelError {
    ReelError::Io(error.kind())
}

impl<W: Write + Seek> Muxer<W> {
    /// Checks the combination and every declared bound, then writes `ftyp`, the placeholder and the
    /// `mdat` header at the current position of `out` (which must be the start of the file).
    pub fn new(mut out: W, spec: MuxSpec, limits: MuxLimits) -> Result<Self, ReelError> {
        check_combination(&spec, &limits)?;
        let video_ts = match &spec.video {
            Some(v) => video_timescale(v.ticks_per_frame)?,
            None => (0, 0),
        };
        let start = out.stream_position().map_err(io_err)?;
        if start != 0 {
            return Err(unsupported("mux-output-not-at-start"));
        }
        let ftyp = ftyp_box(&spec)?;
        out.write_all(&ftyp).map_err(io_err)?;
        let wide_at = u64::try_from(ftyp.len()).map_err(|_| limit("mux-ftyp"))?;
        // `wide` placeholder (becomes the first half of a 64-bit mdat header when needed), then the
        // 32-bit mdat header whose size is patched by `finish`.
        out.write_all(&[0, 0, 0, 8, b'w', b'i', b'd', b'e', 0, 0, 0, 0, b'm', b'd', b'a', b't'])
            .map_err(io_err)?;
        let pos = wide_at.checked_add(16).ok_or(limit("mux-offset"))?;
        Ok(Self {
            out,
            spec,
            limits,
            wide_at,
            pos,
            video: VideoTable::default(),
            audio: AudioTable::default(),
            last: Last::None,
            video_ts,
        })
    }

    pub fn spec(&self) -> &MuxSpec {
        &self.spec
    }

    fn append(&mut self, data: &[u8]) -> Result<u64, ReelError> {
        let len = u64::try_from(data.len()).map_err(|_| limit("mux-sample-bytes"))?;
        let at = self.pos;
        let end = at.checked_add(len).ok_or(limit("mux-offset"))?;
        if end > self.limits.max_file_bytes {
            return Err(limit("mux-file-bytes"));
        }
        self.out.write_all(data).map_err(io_err)?;
        self.pos = end;
        Ok(at)
    }

    /// One video sample (an AV1 temporal unit, an access unit, a VC-3 frame) in decode order.
    /// `pts_index` is its presentation frame index; `sync` marks a random-access sample.
    pub fn write_video(&mut self, data: &[u8], pts_index: u64, sync: bool) -> Result<(), ReelError> {
        if self.spec.video.is_none() {
            return Err(unsupported("mux-no-video-track"));
        }
        if data.is_empty() {
            return Err(unsupported("mux-empty-sample"));
        }
        if data.len() > self.limits.max_sample_bytes {
            return Err(limit("mux-sample-bytes"));
        }
        if self.video.sizes.len() >= self.limits.max_video_samples {
            return Err(limit("mux-video-samples"));
        }
        let size = u32::try_from(data.len()).map_err(|_| limit("mux-sample-bytes"))?;
        let number = u32::try_from(self.video.sizes.len())
            .ok()
            .and_then(|n| n.checked_add(1))
            .ok_or(limit("mux-video-samples"))?;
        let at = self.append(data)?;
        self.video.sizes.push(size);
        self.video.pts.push(pts_index);
        if sync {
            self.video.sync.push(number);
        }
        match (self.last, self.video.chunks.last_mut()) {
            (Last::Video, Some(chunk)) => {
                chunk.samples = chunk.samples.checked_add(1).ok_or(limit("mux-chunk"))?;
            }
            _ => self.video.chunks.push(Chunk {
                offset: at,
                samples: 1,
            }),
        }
        self.last = Last::Video;
        Ok(())
    }

    /// Interleaved little-endian PCM frames in the declared [`PcmSpec`] layout (whole frames only).
    pub fn write_audio(&mut self, data: &[u8]) -> Result<(), ReelError> {
        let Some(pcm) = self.spec.audio else {
            return Err(unsupported("mux-no-audio-track"));
        };
        if data.is_empty() {
            return Ok(());
        }
        let block = usize::try_from(pcm.block_bytes().ok_or(limit("mux-pcm-layout"))?)
            .map_err(|_| limit("mux-pcm-layout"))?;
        if block == 0 || !data.len().is_multiple_of(block) {
            return Err(unsupported("mux-pcm-partial-frame"));
        }
        let frames = data
            .len()
            .checked_div(block)
            .and_then(|f| u32::try_from(f).ok())
            .ok_or(limit("mux-chunk"))?;
        let extend = matches!(
            (self.last, self.audio.chunks.last()),
            (Last::Audio, Some(c)) if c.samples.checked_add(frames).is_some()
        );
        if !extend && self.audio.chunks.len() >= self.limits.max_audio_chunks {
            return Err(limit("mux-audio-chunks"));
        }
        let at = self.append(data)?;
        self.audio.frames = self
            .audio
            .frames
            .checked_add(u64::from(frames))
            .ok_or(limit("mux-audio-frames"))?;
        match (extend, self.audio.chunks.last_mut()) {
            (true, Some(chunk)) => {
                chunk.samples = chunk.samples.checked_add(frames).ok_or(limit("mux-chunk"))?;
            }
            _ => self.audio.chunks.push(Chunk {
                offset: at,
                samples: frames,
            }),
        }
        self.last = Last::Audio;
        Ok(())
    }

    /// Patches the `mdat` header, writes `moov` and returns the output and the receipt.
    pub fn finish(mut self) -> Result<(W, MuxReceipt), ReelError> {
        if self.spec.video.is_some() && self.video.sizes.is_empty() {
            return Err(unsupported("mux-video-track-empty"));
        }
        if self.spec.audio.is_some() && self.audio.frames == 0 {
            return Err(unsupported("mux-audio-track-empty"));
        }
        let delay = reorder_delay(&self.video.pts)?;
        let mdat_start = self.wide_at.checked_add(8).ok_or(limit("mux-offset"))?;
        let mdat_len = self.pos.checked_sub(mdat_start).ok_or(limit("mux-offset"))?;
        let large = mdat_len > u64::from(u32::MAX);
        if large {
            // wide(8) + mdat(8) become one 16-byte header: size 1, 'mdat', 64-bit size.
            let total = self.pos.checked_sub(self.wide_at).ok_or(limit("mux-offset"))?;
            self.out.seek(SeekFrom::Start(self.wide_at)).map_err(io_err)?;
            let mut header = Vec::with_capacity(16);
            header.extend_from_slice(&1u32.to_be_bytes());
            header.extend_from_slice(b"mdat");
            header.extend_from_slice(&total.to_be_bytes());
            self.out.write_all(&header).map_err(io_err)?;
        } else {
            let size = u32::try_from(mdat_len).map_err(|_| limit("mux-offset"))?;
            self.out.seek(SeekFrom::Start(mdat_start)).map_err(io_err)?;
            self.out.write_all(&size.to_be_bytes()).map_err(io_err)?;
        }
        self.out.seek(SeekFrom::Start(self.pos)).map_err(io_err)?;
        let co64 = self.pos > u64::from(u32::MAX);
        let moov = self.moov(delay, co64)?;
        let moov_len = u64::try_from(moov.len()).map_err(|_| limit("mux-moov"))?;
        let file_bytes = self.pos.checked_add(moov_len).ok_or(limit("mux-offset"))?;
        if file_bytes > self.limits.max_file_bytes {
            return Err(limit("mux-file-bytes"));
        }
        self.out.write_all(&moov).map_err(io_err)?;
        self.out.flush().map_err(io_err)?;
        // Media bytes after the 8-byte mdat header (the 64-bit form also covers the 8 placeholder bytes).
        let payload = mdat_len.checked_sub(8).ok_or(limit("mux-offset"))?;
        let receipt = MuxReceipt {
            container: self.spec.container,
            muxer_id: MUXER_ID,
            muxer_version: MUXER_VERSION,
            video_codec: self.spec.video.as_ref().map(|v| v.entry.codec()),
            video_samples: u64::try_from(self.video.sizes.len()).unwrap_or(u64::MAX),
            video_timescale: self.video_ts.0,
            video_sample_delta: self.video_ts.1,
            reorder_delay: delay,
            audio_format: self.spec.audio.map(|a| a.output_name()),
            audio_frames: self.audio.frames,
            mdat_payload_bytes: payload,
            file_bytes,
            co64,
            large_mdat: large,
        };
        Ok((self.out, receipt))
    }

    fn movie_timescale(&self) -> u32 {
        match (&self.spec.video, &self.spec.audio) {
            (Some(_), _) => self.video_ts.0,
            (None, Some(a)) => a.rate,
            (None, None) => 1000,
        }
    }

    fn moov(&self, delay: u64, co64: bool) -> Result<Vec<u8>, ReelError> {
        let movie_ts = self.movie_timescale();
        let mut body = Vec::new();
        let mut durations = Vec::new();
        let mut track_id = 0u32;
        let mut traks = Vec::new();
        if let Some(v) = &self.spec.video {
            track_id = track_id.checked_add(1).ok_or(limit("mux-tracks"))?;
            let samples = u64::try_from(self.video.sizes.len()).map_err(|_| limit("mux-video-samples"))?;
            let media_duration = samples
                .checked_mul(u64::from(self.video_ts.1))
                .ok_or(limit("mux-duration"))?;
            // Movie timescale == video media timescale: the presented duration is exact.
            durations.push(media_duration);
            traks.push(self.video_trak(v, track_id, media_duration, delay, co64)?);
        }
        if let Some(a) = &self.spec.audio {
            track_id = track_id.checked_add(1).ok_or(limit("mux-tracks"))?;
            let movie_duration = rescale_up(self.audio.frames, a.rate, movie_ts)?;
            durations.push(movie_duration);
            traks.push(self.audio_trak(a, track_id, movie_duration, co64)?);
        }
        let movie_duration = durations.iter().copied().max().unwrap_or(0);
        body.extend(mvhd(movie_ts, movie_duration, track_id.checked_add(1).ok_or(limit("mux-tracks"))?)?);
        for t in traks {
            body.extend(t);
        }
        boxed(b"moov", &body)
    }

    fn video_trak(
        &self,
        v: &VideoTrackSpec,
        track_id: u32,
        media_duration: u64,
        delay: u64,
        co64: bool,
    ) -> Result<Vec<u8>, ReelError> {
        let (timescale, delta) = self.video_ts;
        let mut trak = tkhd(track_id, media_duration, false, v.width, v.height)?;
        if delay > 0 {
            let media_time = delay.checked_mul(u64::from(delta)).ok_or(limit("mux-duration"))?;
            trak.extend(edts(media_duration, media_time)?);
        }
        let mut stbl = boxed(b"stsd", &stsd_payload(&visual_entry(self.spec.container, v)?))?;
        stbl.extend(stts_constant(u64::try_from(self.video.sizes.len()).map_err(|_| limit("mux"))?, delta)?);
        if delay > 0 {
            stbl.extend(ctts(&self.video.pts, delay, delta)?);
        }
        if self.video.sync.len() != self.video.sizes.len() {
            stbl.extend(stss(&self.video.sync)?);
        }
        stbl.extend(stsz_table(&self.video.sizes)?);
        stbl.extend(stsc(&self.video.chunks)?);
        stbl.extend(chunk_offsets(&self.video.chunks, co64)?);
        let minf_body = [
            fullbox(b"vmhd", 0, 1, &[0u8; 8])?,
            dinf()?,
            boxed(b"stbl", &stbl)?,
        ]
        .concat();
        let mdia_body = [
            mdhd(timescale, media_duration)?,
            hdlr(self.spec.container, b"vide", "VideoHandler")?,
            boxed(b"minf", &minf_body)?,
        ]
        .concat();
        trak.extend(boxed(b"mdia", &mdia_body)?);
        boxed(b"trak", &trak)
    }

    fn audio_trak(
        &self,
        a: &PcmSpec,
        track_id: u32,
        movie_duration: u64,
        co64: bool,
    ) -> Result<Vec<u8>, ReelError> {
        let mut trak = tkhd(track_id, movie_duration, true, 0, 0)?;
        let block = a.block_bytes().ok_or(limit("mux-pcm-layout"))?;
        let mut stbl = boxed(b"stsd", &stsd_payload(&sound_entry(self.spec.container, a)?))?;
        stbl.extend(stts_constant(self.audio.frames, 1)?);
        stbl.extend(stsz_constant(block, self.audio.frames)?);
        stbl.extend(stsc(&self.audio.chunks)?);
        stbl.extend(chunk_offsets(&self.audio.chunks, co64)?);
        let minf_body = [
            fullbox(b"smhd", 0, 0, &[0u8; 4])?,
            dinf()?,
            boxed(b"stbl", &stbl)?,
        ]
        .concat();
        let mdia_body = [
            mdhd(a.rate, self.audio.frames)?,
            hdlr(self.spec.container, b"soun", "SoundHandler")?,
            boxed(b"minf", &minf_body)?,
        ]
        .concat();
        trak.extend(boxed(b"mdia", &mdia_body)?);
        boxed(b"trak", &trak)
    }
}

/// Frames of decode delay: `max(decode_index - pts_index)`; the presentation indices must be exactly
/// `0..n` (each once).
fn reorder_delay(pts: &[u64]) -> Result<u64, ReelError> {
    let n = pts.len();
    let mut seen = vec![false; n];
    let mut delay = 0u64;
    for (i, &p) in pts.iter().enumerate() {
        let slot = usize::try_from(p)
            .ok()
            .and_then(|p| seen.get_mut(p))
            .ok_or(unsupported("mux-pts-not-a-permutation"))?;
        if *slot {
            return Err(unsupported("mux-pts-not-a-permutation"));
        }
        *slot = true;
        let i = u64::try_from(i).map_err(|_| limit("mux"))?;
        delay = delay.max(i.saturating_sub(p));
    }
    Ok(delay)
}

/// `value` units at `from` per second in `to` units, rounded up.
fn rescale_up(value: u64, from: u32, to: u32) -> Result<u64, ReelError> {
    if from == 0 {
        return Err(unsupported("mux-zero-timescale"));
    }
    let num = u128::from(value)
        .checked_mul(u128::from(to))
        .ok_or(limit("mux-duration"))?;
    let q = num.div_ceil(u128::from(from));
    u64::try_from(q).map_err(|_| limit("mux-duration"))
}

fn boxed(kind: &[u8; 4], payload: &[u8]) -> Result<Vec<u8>, ReelError> {
    let size = payload.len().checked_add(8).ok_or(limit("mux-box"))?;
    let size32 = u32::try_from(size).map_err(|_| limit("mux-box"))?;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&size32.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    Ok(out)
}

fn fullbox(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Result<Vec<u8>, ReelError> {
    let mut body = Vec::with_capacity(payload.len().saturating_add(4));
    body.push(version);
    body.extend_from_slice(&flags.to_be_bytes()[1..]);
    body.extend_from_slice(payload);
    boxed(kind, &body)
}

fn ftyp_box(spec: &MuxSpec) -> Result<Vec<u8>, ReelError> {
    let mut p = Vec::new();
    match spec.container {
        Container::Mp4 => {
            p.extend_from_slice(b"isom");
            p.extend_from_slice(&0x200u32.to_be_bytes());
            p.extend_from_slice(b"isomiso2mp41");
            if matches!(spec.video.as_ref().map(|v| &v.entry), Some(VideoEntry::Av1 { .. })) {
                p.extend_from_slice(b"av01");
            }
            if matches!(spec.video.as_ref().map(|v| &v.entry), Some(VideoEntry::H264Remux { .. })) {
                p.extend_from_slice(b"avc1");
            }
            if matches!(spec.video.as_ref().map(|v| &v.entry), Some(VideoEntry::Hevc { .. })) {
                p.extend_from_slice(b"hvc1");
            }
        }
        Container::Mov => {
            p.extend_from_slice(b"qt  ");
            p.extend_from_slice(&0x2005_0300u32.to_be_bytes());
            p.extend_from_slice(b"qt  ");
        }
    }
    boxed(b"ftyp", &p)
}

/// 3x3 unity matrix of `mvhd`/`tkhd` (16.16, 16.16, 2.30).
const UNITY: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

fn put_unity(p: &mut Vec<u8>) {
    for v in UNITY {
        p.extend_from_slice(&v.to_be_bytes());
    }
}

/// Version 0 when every field fits 32 bits, else version 1.
fn times(p: &mut Vec<u8>, duration: u64) -> u8 {
    // creation and modification times are written as 0 (unknown): output bytes stay deterministic.
    if u32::try_from(duration).is_ok() {
        p.extend_from_slice(&[0u8; 8]);
        0
    } else {
        p.extend_from_slice(&[0u8; 16]);
        1
    }
}

fn put_duration(p: &mut Vec<u8>, version: u8, duration: u64) {
    if version == 0 {
        p.extend_from_slice(&u32::try_from(duration).unwrap_or(u32::MAX).to_be_bytes());
    } else {
        p.extend_from_slice(&duration.to_be_bytes());
    }
}

fn mvhd(timescale: u32, duration: u64, next_track: u32) -> Result<Vec<u8>, ReelError> {
    let mut p = Vec::new();
    let version = times(&mut p, duration);
    p.extend_from_slice(&timescale.to_be_bytes());
    put_duration(&mut p, version, duration);
    p.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
    p.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
    p.extend_from_slice(&[0u8; 10]);
    put_unity(&mut p);
    p.extend_from_slice(&[0u8; 24]); // pre_defined
    p.extend_from_slice(&next_track.to_be_bytes());
    fullbox(b"mvhd", version, 0, &p)
}

fn tkhd(track_id: u32, duration: u64, audio: bool, width: u32, height: u32) -> Result<Vec<u8>, ReelError> {
    let mut p = Vec::new();
    let version = times(&mut p, duration);
    p.extend_from_slice(&track_id.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes());
    put_duration(&mut p, version, duration);
    p.extend_from_slice(&[0u8; 8]);
    p.extend_from_slice(&0u16.to_be_bytes()); // layer
    p.extend_from_slice(&0u16.to_be_bytes()); // alternate group
    p.extend_from_slice(&(if audio { 0x0100u16 } else { 0 }).to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes());
    put_unity(&mut p);
    let fixed = |v: u32| v.checked_mul(0x1_0000).ok_or(limit("mux-dimensions"));
    p.extend_from_slice(&fixed(width)?.to_be_bytes());
    p.extend_from_slice(&fixed(height)?.to_be_bytes());
    fullbox(b"tkhd", version, 0x7, &p)
}

fn edts(segment_duration: u64, media_time: u64) -> Result<Vec<u8>, ReelError> {
    let mut p = Vec::new();
    p.extend_from_slice(&1u32.to_be_bytes());
    let version = match (u32::try_from(segment_duration), i32::try_from(media_time)) {
        (Ok(d), Ok(t)) => {
            p.extend_from_slice(&d.to_be_bytes());
            p.extend_from_slice(&t.to_be_bytes());
            0
        }
        _ => {
            let t = i64::try_from(media_time).map_err(|_| limit("mux-duration"))?;
            p.extend_from_slice(&segment_duration.to_be_bytes());
            p.extend_from_slice(&t.to_be_bytes());
            1
        }
    };
    p.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // media rate 1.0
    let elst = fullbox(b"elst", version, 0, &p)?;
    boxed(b"edts", &elst)
}

fn mdhd(timescale: u32, duration: u64) -> Result<Vec<u8>, ReelError> {
    let mut p = Vec::new();
    let version = times(&mut p, duration);
    p.extend_from_slice(&timescale.to_be_bytes());
    put_duration(&mut p, version, duration);
    p.extend_from_slice(&0x55C4u16.to_be_bytes()); // 'und'
    p.extend_from_slice(&0u16.to_be_bytes());
    fullbox(b"mdhd", version, 0, &p)
}

fn hdlr(container: Container, handler: &[u8; 4], name: &str) -> Result<Vec<u8>, ReelError> {
    let mut p = Vec::new();
    match container {
        Container::Mp4 => p.extend_from_slice(&0u32.to_be_bytes()),
        Container::Mov => p.extend_from_slice(b"mhlr"),
    }
    p.extend_from_slice(handler);
    p.extend_from_slice(&[0u8; 12]);
    match container {
        // ISO: null-terminated UTF-8.
        Container::Mp4 => {
            p.extend_from_slice(name.as_bytes());
            p.push(0);
        }
        // QuickTime: Pascal string.
        Container::Mov => {
            p.push(u8::try_from(name.len()).map_err(|_| limit("mux-hdlr"))?);
            p.extend_from_slice(name.as_bytes());
        }
    }
    fullbox(b"hdlr", 0, 0, &p)
}

fn dinf() -> Result<Vec<u8>, ReelError> {
    let url = fullbox(b"url ", 0, 1, &[])?;
    let mut dref = 1u32.to_be_bytes().to_vec();
    dref.extend(url);
    let dref = fullbox(b"dref", 0, 0, &dref)?;
    boxed(b"dinf", &dref)
}

fn stsd_payload(entry: &[u8]) -> Vec<u8> {
    let mut p = vec![0u8; 4]; // version + flags
    p.extend_from_slice(&1u32.to_be_bytes());
    p.extend_from_slice(entry);
    p
}

fn visual_entry(container: Container, v: &VideoTrackSpec) -> Result<Vec<u8>, ReelError> {
    let mut p = vec![0u8; 6];
    p.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    p.extend_from_slice(&[0u8; 16]); // pre_defined / reserved (QuickTime version, revision, vendor, quality)
    let w = u16::try_from(v.width).map_err(|_| limit("mux-dimensions"))?;
    let h = u16::try_from(v.height).map_err(|_| limit("mux-dimensions"))?;
    p.extend_from_slice(&w.to_be_bytes());
    p.extend_from_slice(&h.to_be_bytes());
    p.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // 72 dpi
    p.extend_from_slice(&0x0048_0000u32.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&1u16.to_be_bytes()); // frame_count
    let mut name = [0u8; 32];
    let label: &[u8] = match &v.entry {
        VideoEntry::Av1 { .. } => b"AV1 (Studio)",
        VideoEntry::H264Remux { .. } => b"H.264 (remux)",
        VideoEntry::Hevc { .. } => b"HEVC (OS encoder)",
        VideoEntry::Vc3 { .. } => b"VC-3 (Studio)",
    };
    name[0] = u8::try_from(label.len()).map_err(|_| limit("mux"))?;
    name[1..=label.len()].copy_from_slice(label);
    p.extend_from_slice(&name);
    p.extend_from_slice(&0x0018u16.to_be_bytes()); // depth
    p.extend_from_slice(&0xFFFFu16.to_be_bytes()); // pre_defined -1
    match &v.entry {
        VideoEntry::Av1 { av1c } => p.extend(boxed(b"av1C", av1c)?),
        VideoEntry::H264Remux { avcc } => p.extend(boxed(b"avcC", avcc)?),
        VideoEntry::Hevc { hvcc } => p.extend(boxed(b"hvcC", hvcc)?),
        VideoEntry::Vc3 { .. } => {}
    }
    if let Some(c) = v.color {
        let mut colr = Vec::new();
        match container {
            Container::Mp4 => colr.extend_from_slice(b"nclx"),
            Container::Mov => colr.extend_from_slice(b"nclc"),
        }
        colr.extend_from_slice(&c.primaries.to_be_bytes());
        colr.extend_from_slice(&c.transfer.to_be_bytes());
        colr.extend_from_slice(&c.matrix.to_be_bytes());
        if container == Container::Mp4 {
            colr.push(if c.full_range { 0x80 } else { 0 });
        }
        p.extend(boxed(b"colr", &colr)?);
    }
    let mut pasp = 1u32.to_be_bytes().to_vec();
    pasp.extend_from_slice(&1u32.to_be_bytes());
    p.extend(boxed(b"pasp", &pasp)?);
    boxed(&v.entry.fourcc(), &p)
}

fn sound_entry(container: Container, a: &PcmSpec) -> Result<Vec<u8>, ReelError> {
    let mut p = vec![0u8; 6];
    p.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    match container {
        Container::Mov => {
            // QuickTime sound description version 2 (`lpcm`).
            p.extend_from_slice(&2u16.to_be_bytes()); // version
            p.extend_from_slice(&0u16.to_be_bytes()); // revision
            p.extend_from_slice(&0u32.to_be_bytes()); // vendor
            p.extend_from_slice(&3u16.to_be_bytes()); // always3
            p.extend_from_slice(&16u16.to_be_bytes()); // always16
            p.extend_from_slice(&0xFFFEu16.to_be_bytes()); // alwaysMinus2
            p.extend_from_slice(&0u16.to_be_bytes()); // always0
            p.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // always65536
            p.extend_from_slice(&72u32.to_be_bytes()); // sizeOfStructOnly
            p.extend_from_slice(&f64::from(a.rate).to_bits().to_be_bytes());
            p.extend_from_slice(&u32::from(a.channels).to_be_bytes());
            p.extend_from_slice(&0x7F00_0000u32.to_be_bytes());
            p.extend_from_slice(&u32::from(a.bits).to_be_bytes());
            // kAudioFormatFlag: packed (0x8), signed integer (0x4) or float (0x1); little-endian (no 0x2).
            let flags: u32 = if a.float { 0x1 | 0x8 } else { 0x4 | 0x8 };
            p.extend_from_slice(&flags.to_be_bytes());
            p.extend_from_slice(&a.block_bytes().ok_or(limit("mux-pcm-layout"))?.to_be_bytes());
            p.extend_from_slice(&1u32.to_be_bytes()); // frames per packet
            boxed(b"lpcm", &p)
        }
        Container::Mp4 => {
            // ISO AudioSampleEntry (version 0) + ISO/IEC 23003-5 PCMConfigurationBox.
            p.extend_from_slice(&[0u8; 8]);
            p.extend_from_slice(&a.channels.to_be_bytes());
            p.extend_from_slice(&a.bits.to_be_bytes());
            p.extend_from_slice(&0u32.to_be_bytes()); // pre_defined + reserved
            let rate = a.rate.checked_mul(0x1_0000).ok_or(unsupported("mux-mp4-pcm-rate-above-65535"))?;
            p.extend_from_slice(&rate.to_be_bytes());
            let bits = u8::try_from(a.bits).map_err(|_| limit("mux-pcm-layout"))?;
            p.extend(fullbox(b"pcmC", 0, 0, &[0x01, bits])?); // format_flags bit 0: little-endian
            boxed(if a.float { b"fpcm" } else { b"ipcm" }, &p)
        }
    }
}

fn stts_constant(count: u64, delta: u32) -> Result<Vec<u8>, ReelError> {
    let count = u32::try_from(count).map_err(|_| limit("mux-samples"))?;
    let mut p = 1u32.to_be_bytes().to_vec();
    p.extend_from_slice(&count.to_be_bytes());
    p.extend_from_slice(&delta.to_be_bytes());
    fullbox(b"stts", 0, 0, &p)
}

fn ctts(pts: &[u64], delay: u64, delta: u32) -> Result<Vec<u8>, ReelError> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for (i, &p) in pts.iter().enumerate() {
        let i = u64::try_from(i).map_err(|_| limit("mux"))?;
        let frames = p
            .checked_add(delay)
            .and_then(|v| v.checked_sub(i))
            .ok_or(limit("mux-ctts"))?;
        let offset = u32::try_from(frames.checked_mul(u64::from(delta)).ok_or(limit("mux-ctts"))?)
            .map_err(|_| limit("mux-ctts"))?;
        match runs.last_mut() {
            Some((n, o)) if *o == offset && *n < u32::MAX => *n = n.saturating_add(1),
            _ => runs.push((1, offset)),
        }
    }
    let mut p = u32::try_from(runs.len()).map_err(|_| limit("mux-ctts"))?.to_be_bytes().to_vec();
    for (n, o) in runs {
        p.extend_from_slice(&n.to_be_bytes());
        p.extend_from_slice(&o.to_be_bytes());
    }
    fullbox(b"ctts", 0, 0, &p)
}

fn stss(sync: &[u32]) -> Result<Vec<u8>, ReelError> {
    let mut p = u32::try_from(sync.len()).map_err(|_| limit("mux-stss"))?.to_be_bytes().to_vec();
    for s in sync {
        p.extend_from_slice(&s.to_be_bytes());
    }
    fullbox(b"stss", 0, 0, &p)
}

fn stsz_table(sizes: &[u32]) -> Result<Vec<u8>, ReelError> {
    let mut p = 0u32.to_be_bytes().to_vec();
    p.extend_from_slice(&u32::try_from(sizes.len()).map_err(|_| limit("mux-stsz"))?.to_be_bytes());
    for s in sizes {
        p.extend_from_slice(&s.to_be_bytes());
    }
    fullbox(b"stsz", 0, 0, &p)
}

fn stsz_constant(size: u32, count: u64) -> Result<Vec<u8>, ReelError> {
    let mut p = size.to_be_bytes().to_vec();
    p.extend_from_slice(&u32::try_from(count).map_err(|_| limit("mux-audio-frames"))?.to_be_bytes());
    fullbox(b"stsz", 0, 0, &p)
}

fn stsc(chunks: &[Chunk]) -> Result<Vec<u8>, ReelError> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        if runs.last().is_some_and(|(_, n)| *n == c.samples) {
            continue;
        }
        let first = u32::try_from(i)
            .ok()
            .and_then(|i| i.checked_add(1))
            .ok_or(limit("mux-chunks"))?;
        runs.push((first, c.samples));
    }
    let mut p = u32::try_from(runs.len()).map_err(|_| limit("mux-chunks"))?.to_be_bytes().to_vec();
    for (first, n) in runs {
        p.extend_from_slice(&first.to_be_bytes());
        p.extend_from_slice(&n.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes()); // sample_description_index
    }
    fullbox(b"stsc", 0, 0, &p)
}

fn chunk_offsets(chunks: &[Chunk], co64: bool) -> Result<Vec<u8>, ReelError> {
    let mut p = u32::try_from(chunks.len()).map_err(|_| limit("mux-chunks"))?.to_be_bytes().to_vec();
    for c in chunks {
        if co64 {
            p.extend_from_slice(&c.offset.to_be_bytes());
        } else {
            let o = u32::try_from(c.offset).map_err(|_| limit("mux-offset"))?;
            p.extend_from_slice(&o.to_be_bytes());
        }
    }
    fullbox(if co64 { b"co64" } else { b"stco" }, 0, 0, &p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_rates_have_exact_timescales() {
        // 25, 29.97, 23.976, 60 fps (ticks per frame from STU-IO-142).
        assert_eq!(video_timescale(10_160_640_000).unwrap(), (25, 1));
        assert_eq!(video_timescale(8_475_667_200).unwrap(), (30000, 1001));
        assert_eq!(video_timescale(10_594_584_000).unwrap(), (24000, 1001));
        assert_eq!(video_timescale(4_233_600_000).unwrap(), (60, 1));
        // A one-tick frame needs the full tick rate as timescale: does not fit 32 bits.
        assert_eq!(
            video_timescale(1).unwrap_err(),
            ReelError::Unsupported {
                what: "mux-frame-rate-not-representable"
            }
        );
    }

    #[test]
    fn reorder_delay_requires_a_permutation() {
        assert_eq!(reorder_delay(&[0, 1, 2]).unwrap(), 0);
        // I P B B: decode order 0 3 1 2 -> sample 2 (pts 1) is one frame late, sample 3 (pts 2) one.
        assert_eq!(reorder_delay(&[0, 3, 1, 2]).unwrap(), 1);
        assert!(reorder_delay(&[0, 0]).is_err());
        assert!(reorder_delay(&[0, 5]).is_err());
        assert!(reorder_delay(&[u64::MAX]).is_err());
    }
}
