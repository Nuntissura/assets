//! Audio file import into a score mixdown, and mixdown export (lanes SMB-AUDIO-IMPORT-WIRING and
//! SMB-AUDIO-IMPORT-2; feature `media`). A WAV, MP3, FLAC, Ogg Vorbis / Opus, ADTS AAC-LC or AIFF file
//! is decoded by reel (`hsk_studio_reel::decode_audio_file`: format detected from the first bytes,
//! refused before allocation above the frame limit), placed as one clip on one audio track of a
//! sequence, and mixed by pulse's [`TimelineMixStream`] (score sessions; the same mixdown the program
//! monitor plays through `audio_out` and the render queue exports). The mix runs at the file's own
//! sample rate, so no resampling happens anywhere on this path. The harness only adapts ports
//! (CX-CAP-001: no codec here).
//!
//! Downmix: a file with more than 2 channels (or any file given an explicit channel map) is folded to
//! stereo with the 3/2 -> 2/0 equations of Rec. ITU-R BS.775-4 Annex 4 Table 2:
//! `L' = 1.0 L + 0.7071 C + 0.7071 LS`, `R' = 1.0 R + 0.7071 C + 0.7071 RS`; LFE is not included
//! (Table 2 has no LFE term). Speakers outside the 3/2 set are first assigned to it (Studio rule, not
//! from the Recommendation): side left / back left -> LS, side right / back right -> RS, back centre
//! -> 0.7071 into both LS and RS, front left / right of centre -> L / R. The speaker of each channel
//! comes from reel's decoder ([`reel::DecodedAudio::speakers`]: the format's channel-order rule or the
//! WAV channel mask) or from [`ImportOptions::channel_map`]; a layout neither defines is the typed
//! refusal `audio_channel_map_required`. [`ImportOptions::normalize_downmix`] scales both outputs by
//! 1 / (largest coefficient sum) so a full-scale signal on every channel cannot exceed full scale.
//!
//! Export streams the whole mix through reel's encode ports: FLAC (`flac` provider, 16 or 24-bit, the
//! STREAMINFO MD5 of the samples written after streaming) or AIFF / AIFF-C (`aiff-pcm` provider: 16 or
//! 24-bit integer, or 32-bit float `fl32`). Integer quantisation is round-to-nearest of
//! `v * 2^(bits-1)`, clamped to the integer range; every clamped or non-finite sample is counted in
//! [`MixExport::clipped_samples`]. Float AIFF is the exact mix (no quantisation).
//!
//! The desktop viewer opens audio files through [`super::Viewer::open`] (an audio panel with export
//! buttons, and the program monitor on the import sequence); see `tests/audio_import.rs`.
#![deny(clippy::arithmetic_side_effects)]

use super::monitor::{self, MonitorFail};
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{
    ClipSchema, ClipSource, ClipTransitions, ContentDigest, SequenceFrameRate, SequenceSchema, Speed,
    StudioClip, StudioSequence, StudioTrack, TrackKind, TrackSchema,
};
use hsk_studio_pulse::{AudioSources, SourceExtents, StreamLimits, TimelineMixStream};
use hsk_studio_reel as reel;
pub use hsk_studio_reel::audio::Speaker;
use serde_json::{Value, json};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Sequence frame rate of an imported file: 25 fps (the audio itself sits on its own sample grid).
pub const IMPORT_TICKS_PER_FRAME: u64 = 10_160_640_000;
/// Clip, track and sequence ids of the one-clip sequence.
pub const IMPORT_SEQUENCE_ID: &str = "seq-audio-import";
pub const IMPORT_TRACK_ID: &str = "STRK-019abcde-0000-7000-8000-00000000a001";
pub const IMPORT_CLIP_ID: &str = "SCLP-019abcde-0000-7000-8000-00000000a001";
/// Rec. ITU-R BS.775-4 Annex 4 Table 2 coefficient for C, LS and RS in the 2/0 equations (as printed).
#[allow(clippy::approx_constant)] // the value printed in Table 2, kept for traceability
pub const BS775_K: f32 = 0.7071;

/// Bounds of one import (authored values).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportLimits {
    /// Largest file read into memory.
    pub max_file_bytes: u64,
    /// Most decoded samples per channel (checked by reel before the PCM buffer is allocated).
    pub max_frames: u64,
}

impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 1 << 30,
            // 3 hours at 48 kHz.
            max_frames: 48_000 * 60 * 60 * 3,
        }
    }
}

/// Import options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportOptions {
    /// Speaker of each file channel, in file order (overrides the decoder's layout; `Unused` drops a
    /// channel). Its length must equal the file's channel count (`audio_channel_map`).
    pub channel_map: Option<Vec<Speaker>>,
    /// Scale the downmix so the largest coefficient sum is 1.
    pub normalize_downmix: bool,
}

/// How a file was folded to stereo.
#[derive(Clone, Debug, PartialEq)]
pub struct Downmix {
    pub speakers: Vec<Speaker>,
    /// `decoder` (reel's layout) or `channel_map` (the caller's).
    pub layout_source: &'static str,
    /// Applied coefficient of each file channel into the left / right output (normalisation included).
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub normalized: bool,
}

/// (left, right) weight of one speaker in the stereo fold (see the module docs).
pub fn bs775_weights(speaker: Speaker) -> (f32, f32) {
    let k = BS775_K;
    match speaker {
        Speaker::FrontLeft | Speaker::FrontLeftOfCenter => (1.0, 0.0),
        Speaker::FrontRight | Speaker::FrontRightOfCenter => (0.0, 1.0),
        Speaker::FrontCenter => (k, k),
        Speaker::BackLeft | Speaker::SideLeft => (k, 0.0),
        Speaker::BackRight | Speaker::SideRight => (0.0, k),
        // Into LS and RS at 0.7071 each, which enter L' / R' at 0.7071.
        Speaker::BackCenter => (k * k, k * k),
        Speaker::LowFrequency | Speaker::Unused => (0.0, 0.0),
    }
}

fn fail(code: &'static str, detail: impl Into<String>) -> MonitorFail {
    MonitorFail {
        code,
        detail: detail.into(),
    }
}

fn reel_fail(e: &reel::ReelError) -> MonitorFail {
    fail("audio_decode", e.to_string())
}

fn pulse_fail(code: &'static str, e: hsk_studio_pulse::PulseError) -> MonitorFail {
    fail(code, e.code())
}

/// A decoded audio file as a clip source of the mixdown (planar, 1 or 2 channels, at the file rate).
#[derive(Clone)]
pub struct ImportedAudio {
    /// `ClipSource::Media` whose content digest is the file's SHA-256 (artifact id `sha256:<hex>`).
    pub source: ClipSource,
    pub sample_rate: u32,
    pub frames: u64,
    /// Channels of the file (before any downmix).
    pub channels_in_file: u16,
    pub planar: Arc<Vec<Vec<f32>>>,
    pub decoder_id: &'static str,
    pub decoder_version: &'static str,
    /// FLAC MD5 of the decoded audio: matched (`Some(true)`), absent (`None`).
    pub checksum_verified: Option<bool>,
    /// The file's speakers as reel reports them (`None`: the format defines no order for the count).
    pub speakers: Option<Vec<Speaker>>,
    pub downmix: Option<Downmix>,
}

impl ImportedAudio {
    /// Length on the timeline: `frames` samples at `sample_rate` in ticks.
    pub fn duration(&self) -> Result<Ticks, MonitorFail> {
        hsk_studio_pulse::ticks_from_samples(self.frames, self.sample_rate)
            .map_err(|e| pulse_fail("audio_rate", e))
    }

    /// The SHA-256 of the file (lower-case hex).
    pub fn content_sha256(&self) -> &str {
        match &self.source {
            ClipSource::Media { content_digest, .. } => &content_digest.digest,
            _ => "",
        }
    }

    /// Structured projection (no paths).
    pub fn to_json(&self) -> Value {
        json!({
            "sha256": self.content_sha256(),
            "sample_rate": self.sample_rate,
            "frames": self.frames,
            "channels_in_file": self.channels_in_file,
            "channels": self.planar.len(),
            "decoder": {"id": self.decoder_id, "version": self.decoder_version},
            "checksum_verified": self.checksum_verified,
            "speakers": self.speakers.as_ref().map(|v| v.iter().map(|s| s.as_str()).collect::<Vec<_>>()),
            "downmix": self.downmix.as_ref().map(|d| json!({
                "rule": "ITU-R BS.775-4 Annex 4 Table 2 (3/2 -> 2/0), LFE omitted",
                "layout_source": d.layout_source,
                "speakers": d.speakers.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                "left": d.left,
                "right": d.right,
                "normalized": d.normalized,
            })),
        })
    }
}

impl AudioSources for ImportedAudio {
    fn audio(&self, source: &ClipSource) -> Option<&[Vec<f32>]> {
        (*source == self.source).then_some(self.planar.as_slice())
    }
}

impl SourceExtents for ImportedAudio {
    fn extent(&self, source: &ClipSource) -> Option<Ticks> {
        (*source == self.source).then(|| self.duration().ok()).flatten()
    }
}

/// Whether `head` (the first bytes of a file) is an audio file the import takes.
pub fn sniff(head: &[u8]) -> bool {
    reel::audio::sniff_audio_file(head)
}

/// Folds `decoded` to stereo (module docs); `None` when it stays as it is (mono, or stereo without
/// a channel map).
type Folded = Option<(Downmix, Vec<Vec<f32>>)>;

fn downmix(decoded: &reel::DecodedAudio, options: &ImportOptions) -> Result<Folded, MonitorFail> {
    let channels = usize::from(decoded.channels);
    let (speakers, layout_source) = match &options.channel_map {
        Some(map) => {
            if map.len() != channels {
                return Err(fail(
                    "audio_channel_map",
                    format!("channel map of {} for {channels} channels", map.len()),
                ));
            }
            (map.clone(), "channel_map")
        }
        None if channels <= 2 => return Ok(None),
        None => match &decoded.speakers {
            Some(s) => (s.clone(), "decoder"),
            None => {
                return Err(fail(
                    "audio_channel_map_required",
                    format!(
                        "{channels} channels from {}: the format defines no channel order; pass ImportOptions::channel_map",
                        decoded.decoder_id
                    ),
                ));
            }
        },
    };
    let weights: Vec<(f32, f32)> = speakers.iter().map(|s| bs775_weights(*s)).collect();
    let sum_l: f32 = weights.iter().map(|w| w.0).sum();
    let sum_r: f32 = weights.iter().map(|w| w.1).sum();
    let peak = sum_l.max(sum_r);
    let scale = if options.normalize_downmix && peak > 1.0 { 1.0 / peak } else { 1.0 };
    let left: Vec<f32> = weights.iter().map(|w| w.0 * scale).collect();
    let right: Vec<f32> = weights.iter().map(|w| w.1 * scale).collect();
    let frames = decoded.frames();
    let mut out = vec![Vec::with_capacity(frames), Vec::with_capacity(frames)];
    for frame in decoded.interleaved.chunks_exact(channels.max(1)) {
        let (mut l, mut r) = (0.0f32, 0.0f32);
        for ((v, a), b) in frame.iter().zip(&left).zip(&right) {
            l += v * a;
            r += v * b;
        }
        out[0].push(l);
        out[1].push(r);
    }
    Ok(Some((
        Downmix {
            speakers,
            layout_source,
            left,
            right,
            normalized: scale != 1.0,
        },
        out,
    )))
}

/// [`import_audio_with`] with default options.
pub fn import_audio(
    src: &mut (impl Read + Seek),
    limits: &ImportLimits,
    cancel: &CancellationToken,
) -> Result<ImportedAudio, MonitorFail> {
    import_audio_with(src, limits, &ImportOptions::default(), cancel)
}

/// Decodes `src` (any audio file reel's whole-file import detects, WAV included) and folds it to stereo
/// when it has more than 2 channels or `options` maps its channels. Refusals, all before the PCM buffer
/// exists unless stated: `audio_decode` (reel code: unknown format, corrupt stream, `limit_exceeded`
/// above `limits.max_frames`), `audio_channel_map` / `audio_channel_map_required` (after decode),
/// `audio_empty` (no samples), `audio_rate` (a rate whose sample period is not a whole number of ticks).
pub fn import_audio_with(
    src: &mut (impl Read + Seek),
    limits: &ImportLimits,
    options: &ImportOptions,
    cancel: &CancellationToken,
) -> Result<ImportedAudio, MonitorFail> {
    import_audio_external(src, limits, options, None, cancel)
}

/// [`import_audio_with`] with an external audio decoder for the codecs it supports (Operator decision
/// D24-A, lane SMB-PR-SOURCE-MONITOR: AAC through the OS decoder, reel-native's `WorkerMfAacProvider`,
/// which the viewer registers under feature `media-native`); `None` is [`import_audio_with`] (AAC is then
/// reel's typed `codec_unavailable` unless feature `studio-aac` is on).
pub fn import_audio_external(
    src: &mut (impl Read + Seek),
    limits: &ImportLimits,
    options: &ImportOptions,
    external: Option<&dyn reel::ExternalAudioDecoder>,
    cancel: &CancellationToken,
) -> Result<ImportedAudio, MonitorFail> {
    let digest = reel::full_digest(src, 1 << 20, cancel).map_err(|e| reel_fail(&e))?;
    src.rewind().map_err(|e| fail("audio_read", e.to_string()))?;
    let decoded = reel::decode_audio_file_with(src, &reel::Limits::default(), limits.max_frames, cancel, external)
        .map_err(|e| reel_fail(&e))?;
    let frames = u64::try_from(decoded.frames()).map_err(|_| fail("audio_decode", "frame count"))?;
    if frames == 0 || decoded.channels == 0 {
        return Err(fail("audio_empty", "no samples"));
    }
    hsk_studio_pulse::ticks_from_samples(frames, decoded.sample_rate).map_err(|e| pulse_fail("audio_rate", e))?;
    let (downmix, planar) = match downmix(&decoded, options)? {
        Some((d, planar)) => (Some(d), planar),
        None => (None, decoded.planar()),
    };
    let hex = reel::hex(&digest);
    Ok(ImportedAudio {
        source: ClipSource::Media {
            artifact_manifest_id: format!("sha256:{hex}"),
            content_digest: ContentDigest {
                algorithm: "sha256".into(),
                digest: hex,
            },
        },
        sample_rate: decoded.sample_rate,
        frames,
        channels_in_file: decoded.channels,
        planar: Arc::new(planar),
        decoder_id: decoded.decoder_id,
        decoder_version: decoded.decoder_version,
        checksum_verified: decoded.checksum_verified,
        speakers: decoded.speakers,
        downmix,
    })
}

/// Reads a file from disk for import; `audio_read` when it cannot be read or exceeds
/// `limits.max_file_bytes` (checked from the metadata before reading).
pub fn read_audio_file(path: &Path, limits: &ImportLimits) -> Result<Vec<u8>, MonitorFail> {
    let len = std::fs::metadata(path)
        .map_err(|e| fail("audio_read", format!("{}: {e}", path.display())))?
        .len();
    if len > limits.max_file_bytes {
        return Err(fail(
            "audio_read",
            format!("{}: {len} bytes exceeds {}", path.display(), limits.max_file_bytes),
        ));
    }
    std::fs::read(path).map_err(|e| fail("audio_read", format!("{}: {e}", path.display())))
}

/// [`open_audio_file_with`] with default options.
pub fn open_audio_file(
    path: &Path,
    limits: &ImportLimits,
    cancel: &CancellationToken,
) -> Result<AudioImport, MonitorFail> {
    open_audio_file_with(path, limits, &ImportOptions::default(), cancel)
}

/// [`import_audio_with`] of a file on disk (see [`read_audio_file`]), placed on a sequence.
pub fn open_audio_file_with(
    path: &Path,
    limits: &ImportLimits,
    options: &ImportOptions,
    cancel: &CancellationToken,
) -> Result<AudioImport, MonitorFail> {
    let bytes = read_audio_file(path, limits)?;
    AudioImport::new(import_audio_with(&mut Cursor::new(bytes), limits, options, cancel)?)
}

/// An imported file on the timeline: a sequence with one audio track holding one clip of the whole
/// file at time zero (unit speed, volume 1, centre pan), and the decoded source behind it.
#[derive(Clone)]
pub struct AudioImport {
    pub sequence: StudioSequence,
    pub audio: ImportedAudio,
}

impl AudioImport {
    pub fn new(audio: ImportedAudio) -> Result<Self, MonitorFail> {
        let duration = audio.duration()?;
        // The sequence ends on the first whole frame at or after the clip end.
        let frames = duration.value().div_ceil(IMPORT_TICKS_PER_FRAME);
        let sequence_end = frames
            .checked_mul(IMPORT_TICKS_PER_FRAME)
            .ok_or_else(|| fail("audio_rate", "sequence length overflows"))?;
        let clip = StudioClip {
            schema_id: ClipSchema::V1,
            clip_id: IMPORT_CLIP_ID.into(),
            source_ref: audio.source.clone(),
            source_in: Ticks::new(0),
            source_out: duration,
            timeline_start: Ticks::new(0),
            duration,
            speed: Speed::UNIT,
            enabled: true,
            link_group: None,
            markers: Vec::new(),
            transitions: ClipTransitions::default(),
        };
        let track = StudioTrack {
            schema_id: TrackSchema::V1,
            track_id: IMPORT_TRACK_ID.into(),
            kind: TrackKind::Audio,
            index: 0,
            name: "A1".into(),
            enabled: true,
            locked: false,
            sync_locked: true,
            targeted: true,
            muted: false,
            soloed: false,
            clips: vec![clip],
            volume: 1.0,
            pan: 0.0,
        };
        let sequence = StudioSequence {
            schema_id: SequenceSchema::V1,
            sequence_id: IMPORT_SEQUENCE_ID.into(),
            name: "audio import".into(),
            frame_rate: SequenceFrameRate {
                ticks_per_frame: IMPORT_TICKS_PER_FRAME,
            },
            duration: Ticks::new(sequence_end),
            tracks: vec![track],
            markers: Vec::new(),
            work_area: None,
        };
        Ok(Self { sequence, audio })
    }

    /// The score mixdown of the sequence at the file's sample rate: hand it to `audio_out::AudioOut`
    /// to play it, or render it (export). `audio_mix` carries pulse's refusal code.
    pub fn mix_stream(
        self,
        max_block_frames: usize,
        cancel: &CancellationToken,
    ) -> Result<TimelineMixStream<ImportedAudio>, MonitorFail> {
        let rate = self.audio.sample_rate;
        let extents = ImportedExtents(self.audio.source.clone(), self.audio.duration()?);
        TimelineMixStream::new(
            &self.sequence,
            rate,
            max_block_frames,
            self.audio,
            &extents,
            StreamLimits::default(),
            cancel,
        )
        .map_err(|e| pulse_fail("audio_mix", e))
    }
}

struct ImportedExtents(ClipSource, Ticks);
impl SourceExtents for ImportedExtents {
    fn extent(&self, source: &ClipSource) -> Option<Ticks> {
        (*source == self.0).then_some(self.1)
    }
}

/// Mixdown export formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixFormat {
    /// FLAC, 16 or 24-bit, STREAMINFO MD5 set.
    Flac { bits: u8 },
    /// AIFF, 16 or 24-bit integer.
    Aiff { bits: u8 },
    /// AIFF-C `fl32`: the mix as 32-bit float.
    AiffFloat,
}

impl MixFormat {
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Flac { .. } => "flac",
            Self::Aiff { .. } => "aiff",
            Self::AiffFloat => "aifc",
        }
    }
}

/// Receipt of one export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MixExport {
    pub format: MixFormat,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
    /// Integer formats: samples clamped to the integer range or non-finite (written as 0).
    pub clipped_samples: u64,
    pub bytes_written: u64,
    /// FLAC: the STREAMINFO MD5 written (lower-case hex).
    pub flac_md5: Option<String>,
}

impl MixExport {
    pub fn to_json(&self) -> Value {
        json!({
            "format": format!("{:?}", self.format),
            "sample_rate": self.sample_rate,
            "channels": self.channels,
            "frames": self.frames,
            "clipped_samples": self.clipped_samples,
            "bytes_written": self.bytes_written,
            "flac_md5": self.flac_md5,
        })
    }
}

/// Most frames one export renders (AIFF declares a 32-bit frame count; FLAC 36-bit).
pub const MAX_EXPORT_FRAMES: u64 = u32::MAX as u64;
const BLOCK: usize = 4096;

fn quantize(v: f32, bits: u8, clipped: &mut u64) -> i32 {
    let max = (1i64 << bits.saturating_sub(1)).saturating_sub(1);
    let min = max.saturating_add(1).saturating_neg();
    if !v.is_finite() {
        *clipped = clipped.saturating_add(1);
        return 0;
    }
    let scaled = (f64::from(v) * (max as f64 + 1.0)).round();
    let q = if scaled > max as f64 {
        *clipped = clipped.saturating_add(1);
        max
    } else if scaled < min as f64 {
        *clipped = clipped.saturating_add(1);
        min
    } else {
        scaled as i64
    };
    i32::try_from(q).unwrap_or(0)
}

/// Planar blocks for the encoders: a timeline mix or a buffer.
pub(crate) trait BlockSource {
    fn channels(&self) -> u16;
    fn rate(&self) -> u32;
    fn frames(&self) -> u64;
    /// Fills `planes[c][..n]` with the next `n` frames.
    fn render(&mut self, planes: &mut [Vec<f32>], n: usize, cancel: &CancellationToken) -> Result<(), MonitorFail>;
}

struct MixSource<S: AudioSources>(TimelineMixStream<S>);
impl<S: AudioSources> BlockSource for MixSource<S> {
    fn channels(&self) -> u16 {
        2
    }
    fn rate(&self) -> u32 {
        hsk_studio_pulse::AudioClock::sample_rate_hz(&self.0)
    }
    fn frames(&self) -> u64 {
        self.0.length_samples()
    }
    fn render(&mut self, planes: &mut [Vec<f32>], n: usize, cancel: &CancellationToken) -> Result<(), MonitorFail> {
        let [l, r] = planes else {
            return Err(fail("audio_mix", "stereo planes"));
        };
        self.0
            .render(&mut l[..n], &mut r[..n], cancel)
            .map_err(|e| pulse_fail("audio_mix", e))
    }
}

/// A buffer of planar samples (1 or 2 channels, equal lengths; the batch queue's export input).
#[cfg_attr(not(feature = "audio-batch"), allow(dead_code))]
pub(crate) struct PlanarSource<'a> {
    pub planes: &'a [Vec<f32>],
    pub rate: u32,
    pub position: usize,
}
impl BlockSource for PlanarSource<'_> {
    fn channels(&self) -> u16 {
        u16::try_from(self.planes.len()).unwrap_or(0)
    }
    fn rate(&self) -> u32 {
        self.rate
    }
    fn frames(&self) -> u64 {
        self.planes.first().map_or(0, |p| p.len() as u64)
    }
    fn render(&mut self, planes: &mut [Vec<f32>], n: usize, _: &CancellationToken) -> Result<(), MonitorFail> {
        let end = self.position.checked_add(n).ok_or_else(|| fail("export_length", "position"))?;
        for (dst, src) in planes.iter_mut().zip(self.planes) {
            let from = src.get(self.position..end).ok_or_else(|| fail("export_length", "past the end"))?;
            dst[..n].copy_from_slice(from);
        }
        self.position = end;
        Ok(())
    }
}

/// Reel encoder input: each chunk is the next rendered block in WAV byte layout (16/24-bit signed
/// little-endian, or 32-bit float little-endian), interleaved.
struct Chunks<'a> {
    src: &'a mut dyn BlockSource,
    planes: Vec<Vec<f32>>,
    block: usize,
    remaining: u64,
    bits: u8,
    float: bool,
    clipped: u64,
    md5: Option<reel::audio::Md5>,
    progress: Option<&'a AtomicU64>,
    failed: Option<MonitorFail>,
    cancel: &'a CancellationToken,
}

impl reel::ChunkSource for Chunks<'_> {
    fn next_chunk(&mut self, buf: &mut Vec<u8>) -> Result<Option<u64>, reel::ReelError> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let n = self.block.min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        if let Err(e) = self.src.render(&mut self.planes, n, self.cancel) {
            self.failed = Some(e);
            return Err(reel::ReelError::Unsupported { what: "mixdown" });
        }
        let width = usize::from(self.bits / 8);
        let start = buf.len();
        for i in 0..n {
            for plane in &self.planes {
                let v = plane[i];
                if self.float {
                    buf.extend_from_slice(&v.to_le_bytes());
                } else {
                    let q = quantize(v, self.bits, &mut self.clipped).to_le_bytes();
                    buf.extend_from_slice(&q[..width]);
                }
            }
        }
        if let Some(md5) = &mut self.md5 {
            md5.update(&buf[start..]);
        }
        let units = u64::try_from(n).unwrap_or(u64::MAX);
        self.remaining = self.remaining.saturating_sub(units);
        if let Some(p) = self.progress {
            p.fetch_add(units, Ordering::AcqRel);
        }
        Ok(Some(units))
    }
}

/// Renders `mix` from its start to its end and streams it to `out` as `format` (see [`export_from`]).
pub fn export_mix<S: AudioSources>(
    mut mix: TimelineMixStream<S>,
    format: MixFormat,
    out: &mut (impl Write + Seek),
    cancel: &CancellationToken,
) -> Result<MixExport, MonitorFail> {
    mix.seek(0);
    let block = BLOCK.min(mix.max_block_frames());
    export_from(&mut MixSource(mix), block, format, out, None, cancel)
}

/// Streams `src` to `out` as `format` through reel's `flac` / `aiff-pcm` encode ports (one block
/// resident at a time). FLAC: the header is written first, the MD5 of the samples is computed while
/// streaming and then written into STREAMINFO at its offset (`out` is seeked back, then to the end).
/// Refused before anything renders: `export_format` (unsupported bit depth or channel count),
/// `export_length` (above [`MAX_EXPORT_FRAMES`]); `audio_mix` for a mix refusal while rendering,
/// `export_write` for an encoder or I/O failure or an incomplete segment.
pub(crate) fn export_from(
    src: &mut dyn BlockSource,
    block: usize,
    format: MixFormat,
    out: &mut (impl Write + Seek),
    progress: Option<&AtomicU64>,
    cancel: &CancellationToken,
) -> Result<MixExport, MonitorFail> {
    let (bits, float, provider, codec) = match format {
        MixFormat::Flac { bits } if matches!(bits, 16 | 24) => {
            (bits, false, reel::AudioFileProvider::flac(), reel::FLAC_PROVIDER_ID)
        }
        MixFormat::Aiff { bits } if matches!(bits, 16 | 24) => {
            (bits, false, reel::AudioFileProvider::aiff(), reel::audio::AIFF_PROVIDER_ID)
        }
        MixFormat::AiffFloat => (32, true, reel::AudioFileProvider::aiff(), reel::audio::AIFF_PROVIDER_ID),
        _ => return Err(fail("export_format", format!("{format:?}: 16 or 24-bit only"))),
    };
    let channels = src.channels();
    if !matches!(channels, 1 | 2) || block == 0 {
        return Err(fail("export_format", format!("{channels} channels")));
    }
    let frames = src.frames();
    if frames > MAX_EXPORT_FRAMES {
        return Err(fail("export_length", format!("{frames} frames exceeds {MAX_EXPORT_FRAMES}")));
    }
    let rate = src.rate();
    let io = |e: std::io::Error| fail("export_write", e.to_string());
    let start = out.stream_position().map_err(io)?;
    let flac = matches!(format, MixFormat::Flac { .. });
    let mut chunks = Chunks {
        src,
        planes: vec![vec![0.0; block]; usize::from(channels)],
        block,
        remaining: frames,
        bits,
        float,
        clipped: 0,
        md5: flac.then(reel::audio::Md5::default),
        progress,
        failed: None,
        cancel,
    };
    let receipt = reel::MediaProvider::encode_segment(
        &provider,
        &reel::EncodeSpec {
            codec: codec.into(),
            params: reel::EncodeParams::Audio {
                rate,
                channels,
                bits: u16::from(bits),
                float,
            },
            total_units: frames,
        },
        &mut chunks,
        out,
        &reel::Limits::default(),
        cancel,
    );
    if let Some(failed) = chunks.failed.take() {
        return Err(failed);
    }
    let receipt = receipt.map_err(|e| {
        if e.code() == reel::ReelError::Canceled.code() {
            fail("canceled", e.to_string())
        } else {
            fail("export_write", e.to_string())
        }
    })?;
    if !receipt.complete {
        return Err(fail("export_write", "incomplete segment"));
    }
    let flac_md5 = match chunks.md5.take() {
        Some(md5) => {
            let digest = md5.finish();
            let at = start
                .checked_add(reel::audio::flac::STREAMINFO_MD5_OFFSET)
                .ok_or_else(|| fail("export_write", "offset"))?;
            out.seek(SeekFrom::Start(at)).map_err(io)?;
            out.write_all(&digest).map_err(io)?;
            out.seek(SeekFrom::End(0)).map_err(io)?;
            Some(reel::hex(&digest))
        }
        None => None,
    };
    Ok(MixExport {
        format,
        sample_rate: rate,
        channels,
        frames,
        clipped_samples: chunks.clipped,
        bytes_written: receipt.bytes_written,
        flac_md5,
    })
}

/// Writes `output` through a sibling temp file renamed over it only when `write` succeeded (the
/// temp file is removed on failure; an existing `output` is replaced only with `overwrite`).
pub(crate) fn write_atomically<T>(
    output: &Path,
    overwrite: bool,
    write: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> Result<T, MonitorFail>,
) -> Result<T, MonitorFail> {
    if output.exists() && !overwrite {
        return Err(fail("output_exists", output.display().to_string()));
    }
    let name = output
        .file_name()
        .ok_or_else(|| fail("output_path", output.display().to_string()))?
        .to_string_lossy()
        .into_owned();
    let temp: PathBuf = output.with_file_name(format!(".{name}.hsk-partial"));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| fail("output_path", format!("{}: {e}", temp.display())))?;
    let mut writer = std::io::BufWriter::new(file);
    let result = write(&mut writer).and_then(|v| {
        writer.flush().map_err(|e| fail("export_write", e.to_string()))?;
        Ok(v)
    });
    drop(writer);
    match result {
        Ok(v) => {
            std::fs::rename(&temp, output).map_err(|e| {
                let _ = std::fs::remove_file(&temp);
                fail("export_write", format!("{}: {e}", output.display()))
            })?;
            Ok(v)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

// ----- desktop viewer -------------------------------------------------------------------------

/// Widget names of the viewer's audio panel.
pub mod widget {
    pub const PANEL: &str = "audio.panel";
    pub const EXPORT_FLAC: &str = "audio.export_flac";
    pub const EXPORT_AIFF: &str = "audio.export_aiff";
}

/// An audio file open in the viewer.
pub struct ViewerAudio {
    pub name: String,
    pub import: AudioImport,
    pub last_export: Option<MixExport>,
}

struct NoVideo;
impl monitor::FrameSource for NoVideo {
    fn frame(&self, _: &ClipSource, _: Ticks) -> Option<Arc<monitor::SourceFrame>> {
        None
    }
}

/// Monitor size of an audio-only import (no picture).
const MONITOR_SIDE: u32 = 16;

impl super::Viewer {
    /// Registers the external audio decoder audio files open with (D24-A: the OS AAC decoder; lane
    /// SMB-PR-SOURCE-MONITOR). Under feature `media-native` [`super::Viewer::new`] already registers
    /// reel-native's `WorkerMfAacProvider` when its worker is beside the executable.
    pub fn set_audio_decoder(&mut self, decoder: Arc<dyn reel::ExternalAudioDecoder>) {
        self.audio_decoder = Some(decoder);
    }

    /// Identity of the registered external audio decoder (`None`: AAC is `codec_unavailable` unless
    /// feature `studio-aac`).
    pub fn audio_decoder(&self) -> Option<reel::DecoderIdentity> {
        self.audio_decoder.as_ref().map(|d| d.identity())
    }

    /// Opens an audio file already read by [`super::Viewer::open`]: the import lands in a one-clip
    /// sequence, the program monitor opens on it (transport at the file rate) and the audio panel shows
    /// it with FLAC / AIFF export. The open document, if any, stays open.
    pub(crate) fn open_audio_bytes(&mut self, path: &Path, bytes: Vec<u8>) -> Result<(), String> {
        let external = self.audio_decoder.clone();
        let import = import_audio_external(&mut Cursor::new(bytes), &ImportLimits::default(), &ImportOptions::default(), external.as_deref(), self.cancel)
            .and_then(AudioImport::new)
            .map_err(|e| format!("audio {}: {} {}", path.display(), e.code, e.detail))?;
        let program = monitor::ProgramMonitor::new(import.sequence.clone(), MONITOR_SIDE, MONITOR_SIDE, monitor::DropPolicy::DropLate)
            .map_err(|e| format!("audio {}: {} {}", path.display(), e.code, e.detail))?;
        let rate = import.audio.sample_rate;
        self.monitor = Some(super::MonitorState {
            monitor: program,
            sources: Some(Box::new(NoVideo)),
            extents: Box::new(import.audio.clone()),
            clock: monitor::WallClock::new(rate),
            texture: None,
            shown: None,
        });
        let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        self.say(format!(
            "audio {name}: {} Hz, {} ch, {} frames ({})",
            rate, import.audio.channels_in_file, import.audio.frames, import.audio.decoder_id
        ));
        if self.save_input.is_empty() {
            self.save_input = path.with_extension("mix.flac").to_string_lossy().into_owned();
        }
        self.source = Some(path.to_path_buf());
        self.audio = Some(ViewerAudio {
            name,
            import,
            last_export: None,
        });
        Ok(())
    }

    /// The open audio file.
    pub fn audio(&self) -> Option<&ViewerAudio> {
        self.audio.as_ref()
    }

    /// The score mixdown of the open audio file, for a host's audio out (`audio_out::AudioOut`).
    pub fn audio_mix_stream(&self, max_block_frames: usize) -> Option<Result<TimelineMixStream<ImportedAudio>, MonitorFail>> {
        self.audio.as_ref().map(|a| a.import.clone().mix_stream(max_block_frames, self.cancel))
    }

    /// Exports the mixdown of the open audio file to `path` (never over the opened file).
    pub fn export_audio(&mut self, path: &Path, format: MixFormat, overwrite: bool) -> Result<MixExport, String> {
        let result = self.export_audio_inner(path, format, overwrite);
        match &result {
            Ok(r) => self.say(format!("exported {} ({} frames, {} clipped)", path.display(), r.frames, r.clipped_samples)),
            Err(e) => self.say(e.clone()),
        }
        result
    }

    fn export_audio_inner(&mut self, path: &Path, format: MixFormat, overwrite: bool) -> Result<MixExport, String> {
        let audio = self.audio.as_ref().ok_or("export: no audio file is open")?;
        if self.source.as_deref() == Some(path) {
            return Err(format!("export {}: refusing to overwrite the opened file", path.display()));
        }
        let mix = audio.import.clone().mix_stream(BLOCK, self.cancel).map_err(|e| format!("export: {} {}", e.code, e.detail))?;
        let receipt = write_atomically(path, overwrite, |w| export_mix(mix, format, w, self.cancel))
            .map_err(|e| format!("export {}: {} {}", path.display(), e.code, e.detail))?;
        if let Some(a) = &mut self.audio {
            a.last_export = Some(receipt.clone());
        }
        Ok(receipt)
    }

    pub(crate) fn audio_json(&self) -> Value {
        self.audio.as_ref().map_or(Value::Null, |a| {
            json!({
                "name": a.name,
                "sequence_id": a.import.sequence.sequence_id,
                "duration_ticks": a.import.sequence.tracks[0].clips[0].duration.value(),
                "import": a.import.audio.to_json(),
                "last_export": a.last_export.as_ref().map(MixExport::to_json),
            })
        })
    }

    pub(crate) fn audio_ui(&mut self, ctx: &egui::Context) {
        let Some(audio) = &self.audio else { return };
        let mut request = None;
        let a = &audio.import.audio;
        let summary = format!(
            "{}\n{} Hz, {} ch in file, {} frames\n{} {}",
            audio.name, a.sample_rate, a.channels_in_file, a.frames, a.decoder_id, a.decoder_version
        );
        let downmix = a.downmix.as_ref().map(|d| format!("downmix BS.775 ({}): {}", d.layout_source, d.speakers.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ")));
        egui::SidePanel::right(super::widget::id(widget::PANEL)).default_width(220.0).show(ctx, |ui| {
            ui.heading("Audio");
            ui.label(summary);
            if let Some(d) = downmix {
                ui.label(d);
            }
            if ui.push_id(super::widget::id(widget::EXPORT_FLAC), |ui| ui.button("Export FLAC")).inner.on_hover_text("24-bit FLAC of the mixdown at the save path with extension .flac").clicked() {
                request = Some(MixFormat::Flac { bits: 24 });
            }
            if ui.push_id(super::widget::id(widget::EXPORT_AIFF), |ui| ui.button("Export AIFF")).inner.on_hover_text("24-bit AIFF of the mixdown at the save path with extension .aiff").clicked() {
                request = Some(MixFormat::Aiff { bits: 24 });
            }
        });
        self.drawn.extend([widget::PANEL, widget::EXPORT_FLAC, widget::EXPORT_AIFF].map(String::from));
        if let Some(format) = request {
            let path = PathBuf::from(self.save_input.trim()).with_extension(format.extension());
            let overwrite = self.overwrite;
            let _ = self.export_audio(&path, format, overwrite);
        }
    }
}
