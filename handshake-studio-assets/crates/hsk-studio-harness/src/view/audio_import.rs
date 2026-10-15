//! Audio file import into a score mixdown, and mixdown export (lane SMB-AUDIO-IMPORT-WIRING; feature
//! `media`). An MP3, FLAC, Ogg Vorbis / Opus, ADTS AAC-LC or AIFF file is decoded by reel
//! (`hsk_studio_reel::decode_audio_file`: format detected from the first bytes, refused before
//! allocation above the frame limit), placed as one clip on one audio track of a sequence, and mixed by
//! pulse's [`TimelineMixStream`] (score sessions; the same mixdown the program monitor plays through
//! `audio_out` and the render queue exports). The mix runs at the file's own sample rate, so no
//! resampling happens anywhere on this path. The harness only adapts ports (CX-CAP-001: no codec here).
//!
//! Export streams the whole mix (stereo) through reel's encode ports: FLAC (`flac` provider, 16 or
//! 24-bit) or AIFF / AIFF-C (`aiff-pcm` provider: 16 or 24-bit integer, or 32-bit float `fl32`). Integer quantisation is round-to-nearest of `v * 2^(bits-1)`, clamped to the
//! integer range; every clamped or non-finite sample is counted in [`MixExport::clipped_samples`].
//! Float AIFF is the exact mix (no quantisation).
#![deny(clippy::arithmetic_side_effects)]

use super::monitor::MonitorFail;
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{
    ClipSchema, ClipSource, ClipTransitions, ContentDigest, SequenceFrameRate, SequenceSchema, Speed,
    StudioClip, StudioSequence, StudioTrack, TrackKind, TrackSchema,
};
use hsk_studio_pulse::{AudioSources, SourceExtents, StreamLimits, TimelineMixStream};
use hsk_studio_reel as reel;
use std::io::{Cursor, Read, Seek, Write};
use std::path::Path;

/// Sequence frame rate of an imported file: 25 fps (the audio itself sits on its own sample grid).
pub const IMPORT_TICKS_PER_FRAME: u64 = 10_160_640_000;
/// Clip, track and sequence ids of the one-clip sequence.
pub const IMPORT_SEQUENCE_ID: &str = "seq-audio-import";
pub const IMPORT_TRACK_ID: &str = "STRK-019abcde-0000-7000-8000-00000000a001";
pub const IMPORT_CLIP_ID: &str = "SCLP-019abcde-0000-7000-8000-00000000a001";

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
pub struct ImportedAudio {
    /// `ClipSource::Media` whose content digest is the file's SHA-256 (artifact id `sha256:<hex>`).
    pub source: ClipSource,
    pub sample_rate: u32,
    pub frames: u64,
    pub planar: Vec<Vec<f32>>,
    pub decoder_id: &'static str,
    pub decoder_version: &'static str,
    /// FLAC MD5 of the decoded audio: matched (`Some(true)`), absent (`None`).
    pub checksum_verified: Option<bool>,
}

impl ImportedAudio {
    /// Length on the timeline: `frames` samples at `sample_rate` in ticks.
    pub fn duration(&self) -> Result<Ticks, MonitorFail> {
        hsk_studio_pulse::ticks_from_samples(self.frames, self.sample_rate)
            .map_err(|e| pulse_fail("audio_rate", e))
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

/// Decodes `src` (any audio file reel's audio providers detect). Refusals, all before the PCM buffer
/// exists: `audio_decode` (reel code: unknown format, corrupt stream, `limit_exceeded` above
/// `limits.max_frames`), `audio_channels` (more than 2 channels: the timeline mix takes mono or stereo
/// sources and no downmix is applied), `audio_empty` (no samples), `audio_rate` (a rate whose sample
/// period is not a whole number of ticks).
pub fn import_audio(
    src: &mut (impl Read + Seek),
    limits: &ImportLimits,
    cancel: &CancellationToken,
) -> Result<ImportedAudio, MonitorFail> {
    let digest = reel::full_digest(src, 1 << 20, cancel).map_err(|e| reel_fail(&e))?;
    src.rewind().map_err(|e| fail("audio_read", e.to_string()))?;
    let decoded = reel::decode_audio_file(src, &reel::Limits::default(), limits.max_frames, cancel)
        .map_err(|e| reel_fail(&e))?;
    if !matches!(decoded.channels, 1 | 2) {
        return Err(fail(
            "audio_channels",
            format!("{} channels (mono or stereo only)", decoded.channels),
        ));
    }
    let frames = u64::try_from(decoded.frames()).map_err(|_| fail("audio_decode", "frame count"))?;
    if frames == 0 {
        return Err(fail("audio_empty", "no samples"));
    }
    hsk_studio_pulse::ticks_from_samples(frames, decoded.sample_rate).map_err(|e| pulse_fail("audio_rate", e))?;
    let hex = reel::hex(&digest);
    let planar = decoded.planar();
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
        planar,
        decoder_id: decoded.decoder_id,
        decoder_version: decoded.decoder_version,
        checksum_verified: decoded.checksum_verified,
    })
}

/// [`import_audio`] of a file on disk; `audio_read` when it cannot be read or exceeds
/// `limits.max_file_bytes` (checked from the metadata before reading).
pub fn open_audio_file(
    path: &Path,
    limits: &ImportLimits,
    cancel: &CancellationToken,
) -> Result<AudioImport, MonitorFail> {
    let len = std::fs::metadata(path)
        .map_err(|e| fail("audio_read", format!("{}: {e}", path.display())))?
        .len();
    if len > limits.max_file_bytes {
        return Err(fail(
            "audio_read",
            format!("{}: {len} bytes exceeds {}", path.display(), limits.max_file_bytes),
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| fail("audio_read", format!("{}: {e}", path.display())))?;
    AudioImport::new(import_audio(&mut Cursor::new(bytes), limits, cancel)?)
}

/// An imported file on the timeline: a sequence with one audio track holding one clip of the whole
/// file at time zero (unit speed, volume 1, centre pan), and the decoded source behind it.
pub struct AudioImport {
    pub sequence: StudioSequence,
    pub audio: ImportedAudio,
}

impl AudioImport {
    pub fn new(audio: ImportedAudio) -> Result<Self, MonitorFail> {
        let duration = audio.duration()?;
        // The sequence ends on the first whole frame at or after the clip end.
        let frames = duration
            .value()
            .div_ceil(IMPORT_TICKS_PER_FRAME);
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

/// Mixdown export formats (stereo).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixFormat {
    /// FLAC, 16 or 24-bit.
    Flac { bits: u8 },
    /// AIFF, 16 or 24-bit integer.
    Aiff { bits: u8 },
    /// AIFF-C `fl32`: the mix as 32-bit float.
    AiffFloat,
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

/// Reel encoder input over the mix: each chunk is the next rendered block in WAV byte layout
/// (16/24-bit signed little-endian, or 32-bit float little-endian), interleaved L R.
struct MixChunks<'a, S: AudioSources> {
    mix: &'a mut TimelineMixStream<S>,
    left: Vec<f32>,
    right: Vec<f32>,
    remaining: u64,
    bits: u8,
    float: bool,
    clipped: u64,
    failed: Option<MonitorFail>,
    cancel: &'a CancellationToken,
}

impl<S: AudioSources> reel::ChunkSource for MixChunks<'_, S> {
    fn next_chunk(&mut self, buf: &mut Vec<u8>) -> Result<Option<u64>, reel::ReelError> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let n = self.left.len().min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        if let Err(e) = self.mix.render(&mut self.left[..n], &mut self.right[..n], self.cancel) {
            self.failed = Some(pulse_fail("audio_mix", e));
            return Err(reel::ReelError::Unsupported { what: "mixdown" });
        }
        let width = usize::from(self.bits / 8);
        for (l, r) in self.left[..n].iter().zip(&self.right[..n]) {
            for v in [*l, *r] {
                if self.float {
                    buf.extend_from_slice(&v.to_le_bytes());
                } else {
                    let q = quantize(v, self.bits, &mut self.clipped).to_le_bytes();
                    buf.extend_from_slice(&q[..width]);
                }
            }
        }
        let units = u64::try_from(n).unwrap_or(u64::MAX);
        self.remaining = self.remaining.saturating_sub(units);
        Ok(Some(units))
    }
}

/// Renders `mix` from its start to its end and streams it to `out` as `format` through reel's
/// `flac` / `aiff-pcm` encode ports (one block of 4096 frames resident at a time). The FLAC header
/// is written before the samples exist, so its STREAMINFO MD5 is "unknown" (all zero, RFC 9639
/// section 8.2). Refused before anything renders: `export_format` (unsupported bit depth),
/// `export_length` (above [`MAX_EXPORT_FRAMES`]); `audio_mix` for a mix refusal while rendering,
/// `export_write` for an encoder or I/O failure or an incomplete segment.
pub fn export_mix<S: AudioSources>(
    mut mix: TimelineMixStream<S>,
    format: MixFormat,
    out: &mut dyn Write,
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
    let frames = mix.length_samples();
    if frames > MAX_EXPORT_FRAMES {
        return Err(fail("export_length", format!("{frames} frames exceeds {MAX_EXPORT_FRAMES}")));
    }
    let rate = hsk_studio_pulse::AudioClock::sample_rate_hz(&mix);
    let block = BLOCK.min(mix.max_block_frames());
    mix.seek(0);
    let mut chunks = MixChunks {
        mix: &mut mix,
        left: vec![0.0; block],
        right: vec![0.0; block],
        remaining: frames,
        bits,
        float,
        clipped: 0,
        failed: None,
        cancel,
    };
    let receipt = reel::MediaProvider::encode_segment(
        &provider,
        &reel::EncodeSpec {
            codec: codec.into(),
            params: reel::EncodeParams::Audio {
                rate,
                channels: 2,
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
    let receipt = receipt.map_err(|e| fail("export_write", e.to_string()))?;
    if !receipt.complete {
        return Err(fail("export_write", "incomplete segment"));
    }
    Ok(MixExport {
        format,
        sample_rate: rate,
        channels: 2,
        frames,
        clipped_samples: chunks.clipped,
        bytes_written: receipt.bytes_written,
    })
}
