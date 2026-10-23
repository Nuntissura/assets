//! Audio streams in Ogg: Vorbis (Vorbis I section 4.2 headers), Opus (RFC 7845) and FLAC (the Ogg FLAC
//! mapping: a 0x7F "FLAC" first packet carrying STREAMINFO, metadata packets, then one frame per packet).
//! The first beginning-of-stream page whose packet names one of these codecs is the stream decoded; the
//! presented length comes from the granule position of the stream's last page (Opus: minus pre-skip).
#![deny(clippy::arithmetic_side_effects)]
use super::engine::StreamShape;
use super::flac::{FlacDecoder, FlacLimits, FrameOutcome, StreamInfo};
use super::ogg::{OggReader, last_granule};
use super::opus::{
    MAX_OPUS_PACKET, OPUS_DECODER_ID, OPUS_DECODER_VERSION, OPUS_RATE, OpusHead, OpusPacketDecoder,
};
use super::packet::{PacketDecoder, PacketSource};
use super::vorbis::{MAX_HEADER_PACKET, VORBIS_DECODER_ID, VORBIS_DECODER_VERSION, VorbisDecoder};
use super::{FLAC_DECODER_ID, FLAC_DECODER_VERSION};
use crate::error::ReelError;
use crate::model::ReadSeek;
use hsk_studio_accord::CancellationToken;

/// Codec of an Ogg audio stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OggCodec {
    Vorbis,
    Opus,
    Flac,
}

impl OggCodec {
    fn sniff(packet: &[u8]) -> Option<Self> {
        if packet.first() == Some(&1) && packet.get(1..7) == Some(b"vorbis") {
            Some(Self::Vorbis)
        } else if packet.starts_with(b"OpusHead") {
            Some(Self::Opus)
        } else if packet.first() == Some(&0x7F) && packet.get(1..5) == Some(b"FLAC") {
            Some(Self::Flac)
        } else {
            None
        }
    }

    pub(crate) fn ids(self) -> (&'static str, &'static str) {
        match self {
            Self::Vorbis => (VORBIS_DECODER_ID, VORBIS_DECODER_VERSION),
            Self::Opus => (OPUS_DECODER_ID, OPUS_DECODER_VERSION),
            Self::Flac => (FLAC_DECODER_ID, FLAC_DECODER_VERSION),
        }
    }
}

/// Name of a codec the first packet of an Ogg stream carries that reel does not decode (for
/// `MEDIA_CODEC_UNAVAILABLE`).
fn unknown_codec_name(packet: &[u8]) -> &'static str {
    if packet.starts_with(b"\x80theora") {
        "theora"
    } else if packet.starts_with(b"Speex   ") {
        "speex"
    } else if packet.starts_with(b"fishead\0") {
        "ogg-skeleton-only"
    } else {
        "ogg-unknown"
    }
}

pub(crate) struct OggPackets<'a>(pub(crate) OggReader<'a>);

impl PacketSource for OggPackets<'_> {
    fn next_packet(&mut self, cancel: &CancellationToken) -> Result<Option<Vec<u8>>, ReelError> {
        self.0.next_packet(cancel)
    }
    fn bytes_read(&self) -> u64 {
        self.0.bytes_read()
    }
}

/// FLAC frames carried one per packet (Ogg FLAC, Matroska A_FLAC, ISO BMFF fLaC).
pub(crate) struct FlacPackets(pub(crate) FlacDecoder);

impl PacketDecoder for FlacPackets {
    fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, ReelError> {
        match self.0.decode_frame(packet, true, out, None)? {
            FrameOutcome::Frame { frames, .. } => Ok(frames),
            FrameOutcome::NeedMore => Err(ReelError::Truncated { delivered: 0 }),
        }
    }
}

pub(crate) struct OggAudio<'a> {
    pub(crate) codec: OggCodec,
    pub(crate) shape: StreamShape,
    pub(crate) packets: OggPackets<'a>,
    pub(crate) decoder: Box<dyn PacketDecoder>,
}

const MAX_AUDIO_PACKET: usize = 1 << 20;

fn header_packet(
    reader: &mut OggReader<'_>,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, ReelError> {
    reader.next_packet(cancel)?.ok_or(ReelError::Corrupt {
        what: "ogg-missing-header",
    })
}

/// Header-only look at the stream: codec, rate, channels, presented total. No audio is decoded.
pub(crate) fn open<'a>(
    src: &'a mut dyn ReadSeek,
    file_len: u64,
    cancel: &CancellationToken,
) -> Result<OggAudio<'a>, ReelError> {
    // Pass 1: which stream, and the granule of its last page.
    let (serial, codec) = {
        let mut reader = OggReader::new(&mut *src, MAX_HEADER_PACKET)?;
        let bos = reader.bos_packets()?;
        let found = bos
            .iter()
            .find_map(|(serial, packet)| OggCodec::sniff(packet).map(|c| (*serial, c)));
        match found {
            Some(found) => found,
            None => {
                let name = bos
                    .first()
                    .map_or("ogg-empty", |(_, p)| unknown_codec_name(p));
                return Err(ReelError::CodecUnavailable { codec: name.into() });
            }
        }
    };
    let granule = last_granule(&mut *src, file_len, serial)?;
    // Pass 2: headers of the selected stream, then the reader stands at the first audio packet.
    let max_packet = match codec {
        OggCodec::Opus => MAX_OPUS_PACKET.max(MAX_HEADER_PACKET),
        _ => MAX_AUDIO_PACKET,
    };
    let mut reader = OggReader::new(src, max_packet)?;
    let bos = reader.bos_packets()?;
    let first = bos
        .into_iter()
        .find(|(s, _)| *s == serial)
        .map(|(_, p)| p)
        .ok_or(ReelError::Corrupt {
            what: "ogg-bos-page",
        })?;
    reader.select(serial);
    let granule_total = granule.and_then(|g| u64::try_from(g).ok());
    let (shape, decoder): (StreamShape, Box<dyn PacketDecoder>) = match codec {
        OggCodec::Vorbis => {
            let comment = header_packet(&mut reader, cancel)?;
            if comment.first() != Some(&3) || comment.get(1..7) != Some(b"vorbis") {
                return Err(ReelError::Corrupt {
                    what: "vorbis-comment-header",
                });
            }
            let setup = header_packet(&mut reader, cancel)?;
            let decoder = VorbisDecoder::new(&first, &setup)?;
            let info = decoder.info;
            (
                StreamShape {
                    rate: info.sample_rate,
                    channels: u16::from(info.channels),
                    total: granule_total,
                    skip: 0,
                },
                Box::new(decoder),
            )
        }
        OggCodec::Opus => {
            let head = OpusHead::parse(&first)?;
            let tags = header_packet(&mut reader, cancel)?;
            if !tags.starts_with(b"OpusTags") {
                return Err(ReelError::Corrupt { what: "opus-tags" });
            }
            let skip = u64::from(head.pre_skip);
            let decoder = OpusPacketDecoder::new(&head)?;
            (
                StreamShape {
                    rate: OPUS_RATE,
                    channels: u16::from(head.channels),
                    total: granule_total.map(|g| g.saturating_sub(skip)),
                    skip,
                },
                Box::new(decoder),
            )
        }
        OggCodec::Flac => {
            // 0x7F "FLAC" major minor, header packet count (BE16), "fLaC", STREAMINFO block.
            if first.get(5) != Some(&1) {
                return Err(ReelError::Unsupported {
                    what: "ogg-flac-mapping-version",
                });
            }
            let count = u16::from_be_bytes([
                first.get(7).copied().unwrap_or(0),
                first.get(8).copied().unwrap_or(0),
            ]);
            if first.get(9..13) != Some(b"fLaC") {
                return Err(ReelError::Corrupt {
                    what: "ogg-flac-header",
                });
            }
            let limits = FlacLimits {
                max_channels: super::MAX_AUDIO_CHANNELS as u8,
                ..FlacLimits::default()
            };
            let body = first.get(17..51).ok_or(ReelError::Corrupt {
                what: "flac-streaminfo",
            })?;
            let info = StreamInfo::parse(body, &limits)?;
            for _ in 0..count {
                header_packet(&mut reader, cancel)?;
            }
            let total = if info.total_samples != 0 {
                Some(info.total_samples)
            } else {
                granule_total
            };
            (
                StreamShape {
                    rate: info.sample_rate,
                    channels: u16::from(info.channels),
                    total,
                    skip: 0,
                },
                Box::new(FlacPackets(FlacDecoder::new(info, limits)?)),
            )
        }
    };
    Ok(OggAudio {
        codec,
        shape,
        packets: OggPackets(reader),
        decoder,
    })
}
