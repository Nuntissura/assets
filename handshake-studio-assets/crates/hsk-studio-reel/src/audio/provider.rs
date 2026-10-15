//! Standalone compressed-audio file providers (one `MediaProvider` per container kind), on the shared
//! delivery loop of `engine`.
#![deny(clippy::arithmetic_side_effects)]
use super::engine::{self, Ids, StreamShape};
use super::flac::{FlacEncoder, FlacLimits, StreamInfo};
use super::native_flac::{NativeFlac, read_header};
use super::ogg_audio;
use super::packet::Decode;
use super::{DecodedAudio, FLAC_DECODER_ID, FLAC_DECODER_VERSION, flac_descriptor};
use crate::error::{ReelError, check_cancel};
use crate::model::*;
use hsk_studio_accord::CancellationToken;
use std::io::{SeekFrom, Write};

/// Provider id of native FLAC files.
pub const FLAC_PROVIDER_ID: &str = "flac";
/// Provider id of Ogg audio files (Vorbis, Opus, FLAC in Ogg).
pub const OGG_PROVIDER_ID: &str = "ogg-audio";

/// Container kinds served by `AudioFileProvider`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFileKind {
    /// Native FLAC stream (`fLaC`).
    Flac,
    /// Ogg (`OggS`) carrying Vorbis, Opus or FLAC.
    Ogg,
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

    /// Every standalone audio file provider of this build, in a stable order.
    pub fn all() -> Vec<Self> {
        vec![Self::flac(), Self::ogg()]
    }

    pub const fn ogg() -> Self {
        Self {
            kind: AudioFileKind::Ogg,
        }
    }

    fn id(&self) -> &'static str {
        match self.kind {
            AudioFileKind::Flac => FLAC_PROVIDER_ID,
            AudioFileKind::Ogg => OGG_PROVIDER_ID,
        }
    }
}

fn flac_limits() -> FlacLimits {
    FlacLimits {
        max_channels: super::MAX_AUDIO_CHANNELS as u8,
        ..FlacLimits::default()
    }
}

fn shape_of(info: &StreamInfo) -> StreamShape {
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
        }
    }

    fn sniff(&self, head: &[u8]) -> bool {
        match self.kind {
            AudioFileKind::Flac => head.starts_with(b"fLaC"),
            AudioFileKind::Ogg => head.starts_with(b"OggS"),
        }
    }

    fn probe(
        &self,
        src: &mut dyn ReadSeek,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<StreamLayout, ReelError> {
        check_cancel(cancel)?;
        let len = source_len(src)?;
        let shape = match self.kind {
            AudioFileKind::Flac => {
                shape_of(&read_header(src, len, limits.max_probe_bytes, &flac_limits())?.info)
            }
            AudioFileKind::Ogg => ogg_audio::open(src, len, cancel)?.shape,
        };
        let tps = crate::time::exact_ticks_per_unit(shape.rate).ok_or(ReelError::Unsupported {
            what: "sample-rate-not-tick-exact",
        })?;
        Ok(StreamLayout {
            kind: MediaKind::Audio,
            codec: self.id().into(),
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
        if req.codec != self.id() {
            return Err(ReelError::CodecUnavailable {
                codec: req.codec.clone(),
            });
        }
        if req.output != OutputMode::Decoded {
            return Err(ReelError::Unsupported {
                what: "audio-file-packets-output",
            });
        }
        let len = verify_grant(src, &req.grant, req.expected_revision)?;
        match self.kind {
            AudioFileKind::Flac => {
                let header = read_header(src, len, limits.max_probe_bytes, &flac_limits())?;
                let shape = shape_of(&header.info);
                // Range and exactness are checked before the first frame is read.
                engine::plan(&shape, req)?;
                let mut source = NativeFlac::open(src, header, flac_limits())?;
                engine::deliver(
                    &mut source,
                    &shape,
                    Ids {
                        codec: FLAC_PROVIDER_ID.into(),
                        decoder_id: FLAC_DECODER_ID,
                        decoder_version: FLAC_DECODER_VERSION,
                    },
                    req,
                    limits,
                    cancel,
                    sink,
                )
            }
            AudioFileKind::Ogg => {
                let ogg = ogg_audio::open(src, len, cancel)?;
                engine::plan(&ogg.shape, req)?;
                let (decoder_id, decoder_version) = ogg.codec.ids();
                let shape = ogg.shape;
                let mut source = Decode {
                    source: ogg.packets,
                    decoder: ogg.decoder,
                };
                engine::deliver(
                    &mut source,
                    &shape,
                    Ids {
                        codec: OGG_PROVIDER_ID.into(),
                        decoder_id,
                        decoder_version,
                    },
                    req,
                    limits,
                    cancel,
                    sink,
                )
            }
        }
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

pub(crate) fn decode_file(
    src: &mut dyn ReadSeek,
    limits: &Limits,
    max_frames: u64,
    cancel: &CancellationToken,
) -> Result<DecodedAudio, ReelError> {
    let len = source_len(src)?;
    let mut head = [0u8; 4];
    let n = std::io::Read::read(src, &mut head)?;
    src.seek(SeekFrom::Start(0))?;
    let head = head.get(..n).unwrap_or(&[]);
    if head.starts_with(b"fLaC") {
        let header = read_header(src, len, limits.max_probe_bytes, &flac_limits())?;
        let shape = shape_of(&header.info);
        let mut source = NativeFlac::open(src, header, flac_limits())?;
        let interleaved = engine::decode_all(&mut source, &shape, max_frames, cancel)?;
        let whole = shape.total.is_none_or(|t| {
            interleaved.len() as u64 == t.saturating_mul(u64::from(shape.channels))
        });
        let checksum_verified = if whole { source.md5_matches() } else { None };
        return Ok(DecodedAudio {
            sample_rate: shape.rate,
            channels: shape.channels,
            interleaved,
            decoder_id: FLAC_DECODER_ID,
            decoder_version: FLAC_DECODER_VERSION,
            checksum_verified,
        });
    }
    if head.starts_with(b"OggS") {
        let ogg = ogg_audio::open(src, len, cancel)?;
        let (decoder_id, decoder_version) = ogg.codec.ids();
        let shape = ogg.shape;
        let mut source = Decode {
            source: ogg.packets,
            decoder: ogg.decoder,
        };
        let interleaved = engine::decode_all(&mut source, &shape, max_frames, cancel)?;
        return Ok(DecodedAudio {
            sample_rate: shape.rate,
            channels: shape.channels,
            interleaved,
            decoder_id,
            decoder_version,
            checksum_verified: None,
        });
    }
    Err(ReelError::ImportUnsupported {
        detected: crate::registry::detect_type(head).to_owned(),
    })
}

pub(crate) fn decode_flac_integers(
    src: &mut dyn ReadSeek,
    limits: &Limits,
    max_frames: u64,
    cancel: &CancellationToken,
) -> Result<(StreamInfo, Vec<i32>, Option<bool>), ReelError> {
    let len = source_len(src)?;
    let header = read_header(src, len, limits.max_probe_bytes, &flac_limits())?;
    let shape = shape_of(&header.info);
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
