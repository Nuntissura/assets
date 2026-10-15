//! Standalone compressed-audio file providers (one `MediaProvider` per container kind), on the shared
//! delivery loop of `engine`. `open_stream` is the one dispatch point from container kind to a decoding
//! `FrameSource`; probe, segment delivery and whole-file decode all go through it.
#![deny(clippy::arithmetic_side_effects)]
use super::aac::{AAC_DECODER_ID, AAC_DECODER_VERSION, AdtsHeader, AdtsStream, probe_adts};
use super::aiff;
use super::engine::{self, FrameSource, Ids, StreamShape};
use super::flac::{FlacEncoder, FlacLimits, StreamInfo};
use super::mp3::{MP3_DECODER_ID, MP3_DECODER_VERSION};
use super::native_flac::{NativeFlac, read_header};
use super::packet::Decode;
use super::speakers::{self, ChannelOrder};
use super::wav_file;
use super::{
    DecodedAudio, FLAC_DECODER_ID, FLAC_DECODER_VERSION, aac_descriptor, flac_descriptor,
    mp3_descriptor, mp3_stream, ogg_audio,
};
use crate::error::{ReelError, check_cancel};
use crate::model::*;
use hsk_studio_accord::CancellationToken;
use std::io::{SeekFrom, Write};

/// Decoder id of the AIFF reader.
pub const AIFF_DECODER_ID: &str = "aiff-pcm";

/// Provider id of native FLAC files.
pub const FLAC_PROVIDER_ID: &str = "flac";
/// Provider id of Ogg audio files (Vorbis, Opus, FLAC in Ogg).
pub const OGG_PROVIDER_ID: &str = "ogg-audio";
/// Provider id of MPEG audio elementary streams (.mp3, .mp2, .mpa).
pub const MP3_PROVIDER_ID: &str = "mpeg-audio";
/// Provider id of ADTS AAC elementary streams (.aac).
pub const AAC_PROVIDER_ID: &str = "aac-adts";
/// Provider id of AIFF / AIFF-C files (decode and encode).
pub const AIFF_PROVIDER_ID: &str = "aiff-pcm";

/// Container kinds served by `AudioFileProvider`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFileKind {
    /// Native FLAC stream (`fLaC`).
    Flac,
    /// Ogg (`OggS`) carrying Vorbis, Opus or FLAC.
    Ogg,
    /// MPEG-1/2/2.5 audio Layer I/II/III elementary stream, optionally after ID3v2 tags.
    MpegAudio,
    /// AAC-LC in ADTS framing.
    AacAdts,
    /// AIFF / AIFF-C (`FORM` .. `AIFF` | `AIFC`).
    AiffPcm,
}

impl AudioFileKind {
    const ALL: [Self; 5] = [
        Self::Flac,
        Self::Ogg,
        Self::MpegAudio,
        Self::AacAdts,
        Self::AiffPcm,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::Flac => FLAC_PROVIDER_ID,
            Self::Ogg => OGG_PROVIDER_ID,
            Self::MpegAudio => MP3_PROVIDER_ID,
            Self::AacAdts => AAC_PROVIDER_ID,
            Self::AiffPcm => AIFF_PROVIDER_ID,
        }
    }

    fn sniff(self, head: &[u8]) -> bool {
        match self {
            Self::Flac => head.starts_with(b"fLaC"),
            Self::Ogg => head.starts_with(b"OggS"),
            Self::MpegAudio => {
                head.starts_with(b"ID3")
                    || head
                        .get(..4)
                        .and_then(|h| <[u8; 4]>::try_from(h).ok())
                        .is_some_and(|h| super::mp3::header_is_valid(&h))
            }
            Self::AacAdts => AdtsHeader::parse(head).is_some(),
            Self::AiffPcm => {
                head.starts_with(b"FORM")
                    && matches!(head.get(8..12), Some(b"AIFF") | Some(b"AIFC"))
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AudioFileProvider {
    pub kind: AudioFileKind,
}

impl AudioFileProvider {
    pub const fn flac() -> Self {
        Self {
            kind: AudioFileKind::Flac,
        }
    }

    pub const fn ogg() -> Self {
        Self {
            kind: AudioFileKind::Ogg,
        }
    }

    pub const fn aac_adts() -> Self {
        Self {
            kind: AudioFileKind::AacAdts,
        }
    }

    pub const fn aiff() -> Self {
        Self {
            kind: AudioFileKind::AiffPcm,
        }
    }

    pub const fn mpeg_audio() -> Self {
        Self {
            kind: AudioFileKind::MpegAudio,
        }
    }

    /// Every standalone audio file provider of this build, in a stable order.
    pub fn all() -> Vec<Self> {
        AudioFileKind::ALL
            .iter()
            .map(|&kind| Self { kind })
            .collect()
    }
}

pub(crate) fn flac_limits() -> FlacLimits {
    FlacLimits {
        max_channels: super::MAX_AUDIO_CHANNELS as u8,
        ..FlacLimits::default()
    }
}

pub(crate) fn flac_shape(info: &StreamInfo) -> StreamShape {
    StreamShape {
        rate: info.sample_rate,
        channels: u16::from(info.channels),
        total: (info.total_samples != 0).then_some(info.total_samples),
        skip: 0,
    }
}

fn source_len(src: &mut dyn ReadSeek) -> Result<u64, ReelError> {
    let len = src.seek(SeekFrom::End(0))?;
    src.seek(SeekFrom::Start(0))?;
    Ok(len)
}

/// An opened stream: its shape, its decoder ids and the decoding frame source (positioned at the first
/// audio unit). Only headers have been read.
pub(crate) struct Opened<'a> {
    pub(crate) shape: StreamShape,
    pub(crate) decoder_id: &'static str,
    pub(crate) decoder_version: &'static str,
    pub(crate) source: Box<dyn FrameSource + 'a>,
    /// FLAC: the native reader, kept to verify the MD5 after a whole-stream decode.
    flac: Option<NativeFlac<'a>>,
}

impl Opened<'_> {
    fn frame_source(&mut self) -> &mut dyn FrameSource {
        match self.flac.as_mut() {
            Some(flac) => flac,
            None => self.source.as_mut(),
        }
    }
}

struct NoFrames;
impl FrameSource for NoFrames {
    fn next_block(
        &mut self,
        _out: &mut Vec<f32>,
        _cancel: &CancellationToken,
    ) -> Result<Option<usize>, ReelError> {
        Ok(None)
    }
    fn bytes_read(&self) -> u64 {
        0
    }
}

pub(crate) fn open_stream<'a>(
    kind: AudioFileKind,
    src: &'a mut dyn ReadSeek,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Opened<'a>, ReelError> {
    let len = source_len(src)?;
    match kind {
        AudioFileKind::Flac => {
            let header = read_header(src, len, limits.max_probe_bytes, &flac_limits())?;
            let shape = flac_shape(&header.info);
            Ok(Opened {
                shape,
                decoder_id: FLAC_DECODER_ID,
                decoder_version: FLAC_DECODER_VERSION,
                source: Box::new(NoFrames),
                flac: Some(NativeFlac::open(src, header, flac_limits())?),
            })
        }
        AudioFileKind::Ogg => {
            let ogg = ogg_audio::open(src, len, cancel)?;
            let (decoder_id, decoder_version) = ogg.codec.ids();
            Ok(Opened {
                shape: ogg.shape,
                decoder_id,
                decoder_version,
                source: Box::new(Decode {
                    source: ogg.packets,
                    decoder: ogg.decoder,
                }),
                flac: None,
            })
        }
        AudioFileKind::MpegAudio => {
            let info = mp3_stream::probe(src, len, limits, cancel)?;
            Ok(Opened {
                shape: info.shape,
                decoder_id: MP3_DECODER_ID,
                decoder_version: MP3_DECODER_VERSION,
                source: Box::new(mp3_stream::Mp3Stream::open(src, info)?),
                flac: None,
            })
        }
        AudioFileKind::AacAdts => {
            let info = probe_adts(src, len, limits, cancel)?;
            Ok(Opened {
                shape: info.shape,
                decoder_id: AAC_DECODER_ID,
                decoder_version: AAC_DECODER_VERSION,
                source: Box::new(AdtsStream::open(src, &info)?),
                flac: None,
            })
        }
        AudioFileKind::AiffPcm => {
            let info = aiff::parse(src, len, limits)?;
            Ok(Opened {
                shape: aiff::shape(&info),
                decoder_id: AIFF_DECODER_ID,
                decoder_version: "1",
                source: Box::new(aiff::AiffReader::open(src, info)?),
                flac: None,
            })
        }
    }
}

impl MediaProvider for AudioFileProvider {
    fn descriptor(&self) -> CodecDescriptor {
        match self.kind {
            AudioFileKind::Flac => CodecDescriptor {
                id: FLAC_PROVIDER_ID,
                container: "flac",
                ..flac_descriptor()
            },
            AudioFileKind::Ogg => CodecDescriptor {
                id: OGG_PROVIDER_ID,
                name: "Ogg audio (Vorbis, Opus, FLAC)",
                kind: MediaKind::Audio,
                container: "ogg",
                bit_depths: &[16, 24, 32],
                chroma: None,
                transfer: None,
                decoder_id: "ogg-rfc3533",
                decoder_version: "1",
                licence: STUDIO_OWNED_LICENCE,
                certification: Certification::NotApplicable,
                can_decode: true,
                can_encode: false,
            },
            AudioFileKind::MpegAudio => CodecDescriptor {
                id: MP3_PROVIDER_ID,
                container: "mpeg-audio",
                ..mp3_descriptor()
            },
            AudioFileKind::AacAdts => CodecDescriptor {
                id: AAC_PROVIDER_ID,
                container: "adts",
                ..aac_descriptor()
            },
            AudioFileKind::AiffPcm => CodecDescriptor {
                id: AIFF_PROVIDER_ID,
                name: "AIFF / AIFF-C PCM (read: integer, float, G.711; write: integer, float)",
                kind: MediaKind::Audio,
                container: "aiff",
                bit_depths: &[8, 16, 24, 32, 64],
                chroma: None,
                transfer: None,
                decoder_id: AIFF_DECODER_ID,
                decoder_version: "1",
                licence: STUDIO_OWNED_LICENCE,
                certification: Certification::NotApplicable,
                can_decode: true,
                can_encode: true,
            },
        }
    }

    fn sniff(&self, head: &[u8]) -> bool {
        self.kind.sniff(head)
    }

    fn probe(
        &self,
        src: &mut dyn ReadSeek,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<StreamLayout, ReelError> {
        check_cancel(cancel)?;
        let shape = open_stream(self.kind, src, limits, cancel)?.shape;
        let tps = crate::time::exact_ticks_per_unit(shape.rate).ok_or(ReelError::Unsupported {
            what: "sample-rate-not-tick-exact",
        })?;
        Ok(StreamLayout {
            kind: MediaKind::Audio,
            codec: self.kind.id().into(),
            width: 0,
            height: 0,
            ticks_per_unit: tps,
            units: shape.total.unwrap_or(0),
            channels: shape.channels,
            tracks: 1,
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
        check_cancel(cancel)?;
        if req.codec != self.kind.id() {
            return Err(ReelError::CodecUnavailable {
                codec: req.codec.clone(),
            });
        }
        if req.output != OutputMode::Decoded {
            return Err(ReelError::Unsupported {
                what: "audio-file-packets-output",
            });
        }
        verify_grant(src, &req.grant, req.expected_revision)?;
        let mut opened = open_stream(self.kind, src, limits, cancel)?;
        // Range and exactness are checked before the first audio unit is read.
        engine::plan(&opened.shape, req)?;
        let shape = opened.shape;
        let ids = Ids {
            codec: self.kind.id().into(),
            decoder_id: opened.decoder_id,
            decoder_version: opened.decoder_version,
        };
        engine::deliver(
            opened.frame_source(),
            &shape,
            ids,
            req,
            limits,
            cancel,
            sink,
        )
    }

    /// FLAC encode. Source chunks carry interleaved PCM in WAV byte layout: 8-bit unsigned, 16/24-bit
    /// signed little-endian (`EncodeParams::Audio { bits: 8 | 16 | 24, float: false }`). The stream
    /// header is written first, so STREAMINFO declares `total_units` samples and leaves MD5 and frame
    /// sizes unknown (all zero, RFC 9639 section 8.2); `encode_flac` writes them when the caller holds
    /// the samples.
    fn encode_segment(
        &self,
        spec: &EncodeSpec,
        source: &mut dyn ChunkSource,
        out: &mut dyn Write,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<EncodeReceipt, ReelError> {
        if self.kind == AudioFileKind::AiffPcm && spec.codec == AIFF_PROVIDER_ID {
            return aiff::encode(spec, source, out, limits, cancel, AIFF_PROVIDER_ID);
        }
        if self.kind != AudioFileKind::Flac || spec.codec != FLAC_PROVIDER_ID {
            return Err(ReelError::CodecUnavailable {
                codec: spec.codec.clone(),
            });
        }
        let EncodeParams::Audio {
            rate,
            channels,
            bits,
            float,
        } = spec.params
        else {
            return Err(ReelError::Unsupported {
                what: "flac-video-params",
            });
        };
        if float || !matches!(bits, 8 | 16 | 24) || channels == 0 || channels > 8 {
            return Err(ReelError::Unsupported {
                what: "flac-encode-format",
            });
        }
        if spec.total_units >= 1 << 36 {
            return Err(ReelError::LimitExceeded {
                what: "flac-total-samples",
            });
        }
        let mut encoder = FlacEncoder::new(rate, channels as u8, bits as u8)?;
        let header = encoder.stream_header(spec.total_units);
        out.write_all(&header)?;
        let mut bytes_written = header.len() as u64;
        let bytes_per = usize::from(bits / 8);
        let block = usize::from(channels).saturating_mul(bytes_per);
        let (mut buf, mut ints, mut frames) = (Vec::new(), Vec::new(), Vec::new());
        let (mut written, mut canceled) = (0u64, false);
        loop {
            if check_cancel(cancel).is_err() {
                canceled = true;
                break;
            }
            buf.clear();
            let Some(units) = source.next_chunk(&mut buf)? else {
                break;
            };
            let expected = usize::try_from(units)
                .ok()
                .and_then(|u| u.checked_mul(block));
            if expected != Some(buf.len()) {
                return Err(ReelError::Corrupt {
                    what: "chunk-size-mismatch",
                });
            }
            if buf.len() > limits.max_chunk_bytes {
                return Err(ReelError::LimitExceeded {
                    what: "chunk-bytes",
                });
            }
            written = written
                .checked_add(units)
                .filter(|t| *t <= spec.total_units)
                .ok_or(ReelError::LimitExceeded {
                    what: "source-exceeds-declared",
                })?;
            ints.clear();
            for s in buf.chunks_exact(bytes_per) {
                ints.push(match s {
                    [a] => i32::from(*a).wrapping_sub(128),
                    [a, b] => i32::from(i16::from_le_bytes([*a, *b])),
                    [a, b, c] => i32::from_le_bytes([0, *a, *b, *c]) >> 8,
                    _ => 0,
                });
            }
            frames.clear();
            encoder.push(&ints, &mut frames)?;
            out.write_all(&frames)?;
            bytes_written = bytes_written.saturating_add(frames.len() as u64);
        }
        let complete = !canceled && written == spec.total_units;
        if complete {
            frames.clear();
            encoder.finish(&mut frames)?;
            out.write_all(&frames)?;
            bytes_written = bytes_written.saturating_add(frames.len() as u64);
        }
        Ok(EncodeReceipt {
            codec: FLAC_PROVIDER_ID.into(),
            complete,
            units_written: written,
            units_declared: spec.total_units,
            bytes_written,
            canceled,
        })
    }
}

/// The audio file kind of a source, from its first bytes.
/// Whether `head` (a file's first bytes, 16 are enough) is a file `decode_audio_file` takes: WAV or one
/// of the standalone audio formats.
pub fn sniff_audio_file(head: &[u8]) -> bool {
    wav_file::sniff(head) || AudioFileKind::ALL.into_iter().any(|k| k.sniff(head))
}

pub(crate) fn detect_kind(src: &mut dyn ReadSeek) -> Result<AudioFileKind, ReelError> {
    let mut head = [0u8; 16];
    src.seek(SeekFrom::Start(0))?;
    let mut n = 0usize;
    while n < head.len() {
        let got = std::io::Read::read(src, head.get_mut(n..).unwrap_or(&mut []))?;
        if got == 0 {
            break;
        }
        n = n.saturating_add(got);
    }
    src.seek(SeekFrom::Start(0))?;
    let head = head.get(..n).unwrap_or(&[]);
    AudioFileKind::ALL
        .into_iter()
        .find(|k| k.sniff(head))
        .ok_or_else(|| ReelError::ImportUnsupported {
            detected: crate::registry::detect_type(head).to_owned(),
        })
}

pub(crate) fn decode_file(
    src: &mut dyn ReadSeek,
    limits: &Limits,
    max_frames: u64,
    cancel: &CancellationToken,
) -> Result<DecodedAudio, ReelError> {
    if let Some(decoded) = decode_wav_file(src, limits, max_frames, cancel)? {
        return Ok(decoded);
    }
    let kind = detect_kind(src)?;
    let mut opened = open_stream(kind, src, limits, cancel)?;
    let shape = opened.shape;
    let interleaved = engine::decode_all(opened.frame_source(), &shape, max_frames, cancel)?;
    let whole = shape
        .total
        .is_none_or(|t| interleaved.len() as u64 == t.saturating_mul(u64::from(shape.channels)));
    let checksum_verified = match opened.flac.take() {
        Some(flac) if whole => flac.md5_matches(),
        _ => None,
    };
    Ok(DecodedAudio {
        sample_rate: shape.rate,
        channels: shape.channels,
        interleaved,
        decoder_id: opened.decoder_id,
        decoder_version: opened.decoder_version,
        checksum_verified,
        speakers: speakers::layout_for(channel_order(opened.decoder_id), shape.channels),
    })
}

/// Channel-order rule of a decoder's output (see `speakers`).
fn channel_order(decoder_id: &str) -> ChannelOrder {
    match decoder_id {
        FLAC_DECODER_ID => ChannelOrder::Flac,
        super::VORBIS_DECODER_ID | super::OPUS_DECODER_ID => ChannelOrder::Vorbis,
        AAC_DECODER_ID => ChannelOrder::Aac,
        _ => ChannelOrder::MonoStereo,
    }
}

/// A RIFF/WAVE file through the whole-file import (`None` when the bytes are not WAV).
fn decode_wav_file(
    src: &mut dyn ReadSeek,
    limits: &Limits,
    max_frames: u64,
    cancel: &CancellationToken,
) -> Result<Option<DecodedAudio>, ReelError> {
    let len = source_len(src)?;
    let mut head = [0u8; 12];
    src.seek(SeekFrom::Start(0))?;
    let mut n = 0usize;
    while n < head.len() {
        let got = std::io::Read::read(src, head.get_mut(n..).unwrap_or(&mut []))?;
        if got == 0 {
            break;
        }
        n = n.saturating_add(got);
    }
    src.seek(SeekFrom::Start(0))?;
    if !wav_file::sniff(head.get(..n).unwrap_or(&[])) {
        return Ok(None);
    }
    let (info, mask) = wav_file::parse(src, len, limits, cancel)?;
    let shape = aiff::shape(&info);
    let mut reader = aiff::AiffReader::open(src, info)?;
    let interleaved = engine::decode_all(&mut reader, &shape, max_frames, cancel)?;
    Ok(Some(DecodedAudio {
        sample_rate: shape.rate,
        channels: shape.channels,
        interleaved,
        decoder_id: wav_file::WAV_FILE_DECODER_ID,
        decoder_version: "1",
        checksum_verified: None,
        speakers: speakers::layout_for(ChannelOrder::Wav(mask), shape.channels),
    }))
}

pub(crate) fn decode_flac_integers(
    src: &mut dyn ReadSeek,
    limits: &Limits,
    max_frames: u64,
    cancel: &CancellationToken,
) -> Result<(StreamInfo, Vec<i32>, Option<bool>), ReelError> {
    let len = source_len(src)?;
    let header = read_header(src, len, limits.max_probe_bytes, &flac_limits())?;
    let shape = flac_shape(&header.info);
    let mut source = NativeFlac::open(src, header, flac_limits())?;
    source.keep_integers();
    let decoded = engine::decode_all(&mut source, &shape, max_frames, cancel)?;
    let ints = source.take_integers();
    if ints.len() != decoded.len() {
        return Err(ReelError::Corrupt {
            what: "flac-integer-count",
        });
    }
    Ok((header.info, ints, source.md5_matches()))
}
