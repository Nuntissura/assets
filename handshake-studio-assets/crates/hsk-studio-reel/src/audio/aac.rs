//! AAC-LC: ADTS framing (ISO/IEC 13818-7 / 14496-3 subclause 1.A.2), AudioSpecificConfig parsing and the
//! MP4 `esds` reader, all bounded here. Decoding (Operator decision D24-A, `aac_decoder`): the registered
//! external decoder (the OS decoder in reel-native's worker) when there is one, else the Studio-owned LC
//! decoder of `aac_lc` (channel configurations 1-7) with cargo feature `studio-aac` (default off), else
//! `MEDIA_CODEC_UNAVAILABLE aac-lc`. Every Studio decoder call runs behind `packet::guarded`. History: soundkit-aac-lc 0.12.3
//! (MIT) was tried first and rejected on evidence: its intensity-stereo sign is inverted (codebook 14
//! read as +1, ISO/IEC 14496-3 4.6.8.2.3 gives INTENSITY_HCB2 = 14 the sign -1) and it inverts again
//! for ms_mask_present == 2, which the standard does not; on an ffmpeg-encoded stereo file this cost
//! 14-57 dB at the frames using intensity stereo, where libavcodec's float and fixed decoders agree within
//! 1 LSB. Its Huffman and band tables are reused (MIT, crate NOTICE).
#![deny(clippy::arithmetic_side_effects)]
#[cfg(feature = "studio-aac")]
use super::aac_lc::AacLc;
use super::bits::Bits;
use super::engine::{FrameSource, StreamShape};
use super::external::{AudioCodec, AudioStreamSpec, Checked, ExternalAudioDecoder};
use super::packet::PacketDecoder;
#[cfg(feature = "studio-aac")]
use super::packet::guarded;
use super::{MAX_AUDIO_CHANNELS, MAX_SAMPLE_RATE};
use crate::error::{ReelError, check_cancel};
use crate::model::{Limits, ReadSeek};
use hsk_studio_accord::CancellationToken;
use std::io::SeekFrom;

/// Studio AAC-LC decoder id (receipts carry it as `decoder_id`; cargo feature `studio-aac`).
pub const AAC_DECODER_ID: &str = "aac-lc-iso14496-3";
pub const AAC_DECODER_VERSION: &str = "1";

const FRAME: usize = super::external::FRAMES_PER_ACCESS_UNIT;
/// Largest ADTS frame: the 13-bit frame_length field.
const MAX_ADTS_FRAME: usize = 8191;

const SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// Sampling rate, channel count and the 2-byte AudioSpecificConfig of an ADTS header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AdtsHeader {
    pub(crate) header_len: usize,
    pub(crate) frame_len: usize,
    pub(crate) object_type: u8,
    pub(crate) sf_index: u8,
    pub(crate) channel_config: u8,
    pub(crate) raw_blocks: u8,
}

impl AdtsHeader {
    pub(crate) fn parse(h: &[u8]) -> Option<Self> {
        let b = |i: usize| h.get(i).copied();
        let (b0, b1, b2, b3, b4, b5, b6) = (b(0)?, b(1)?, b(2)?, b(3)?, b(4)?, b(5)?, b(6)?);
        // 12-bit sync and layer 00.
        if b0 != 0xFF || b1 & 0xF6 != 0xF0 {
            return None;
        }
        let protection_absent = b1 & 1 == 1;
        let profile = b2 >> 6;
        let sf_index = (b2 >> 2) & 0x0F;
        let channel_config = ((b2 & 1) << 2) | (b3 >> 6);
        let frame_len = (usize::from(b3 & 3) << 11) | (usize::from(b4) << 3) | usize::from(b5 >> 5);
        let raw_blocks = b6 & 3;
        let header_len = if protection_absent { 7 } else { 9 };
        if usize::from(sf_index) >= SAMPLE_RATES.len() || frame_len < header_len {
            return None;
        }
        Some(Self {
            header_len,
            frame_len,
            object_type: profile.saturating_add(1),
            sf_index,
            channel_config,
            raw_blocks,
        })
    }

    /// AudioSpecificConfig: object type (5), frequency index (4), channel configuration (4),
    /// GASpecificConfig frameLengthFlag 0, dependsOnCoreCoder 0, extensionFlag 0.
    pub(crate) fn audio_specific_config(&self) -> [u8; 2] {
        let v: u16 = (u16::from(self.object_type & 0x1F) << 11)
            | (u16::from(self.sf_index & 0x0F) << 7)
            | (u16::from(self.channel_config & 0x0F) << 3);
        v.to_be_bytes()
    }

    /// Compatible with the stream's first header (same object type, rate and channels).
    fn continues(&self, first: &Self) -> bool {
        self.object_type == first.object_type
            && self.sf_index == first.sf_index
            && self.channel_config == first.channel_config
    }
}

/// Fields of an AudioSpecificConfig (ISO/IEC 14496-3 subclause 1.6.2.1) that the LC decoder needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacConfig {
    pub object_type: u8,
    pub sf_index: u8,
    pub sample_rate: u32,
    pub channel_config: u8,
    pub channels: u16,
}

/// Parse and check an AudioSpecificConfig: AAC-LC only (object type 2; explicit SBR / PS signalling,
/// 960-sample frames, program-config channel layouts and explicit sampling rates are typed refusals),
/// rate and channel count within the audio bounds, before any decoder allocates for it.
pub fn parse_audio_specific_config(asc: &[u8]) -> Result<AacConfig, ReelError> {
    let mut b = Bits::new(asc);
    let bad = || ReelError::Corrupt {
        what: "aac-audio-specific-config",
    };
    let mut aot = b.read(5).ok_or_else(bad)?;
    if aot == 31 {
        aot = b.read(6).ok_or_else(bad)?.saturating_add(32);
    }
    let sf_index = b.read(4).ok_or_else(bad)? as u8;
    if sf_index == 15 {
        return Err(ReelError::Unsupported {
            what: "aac-explicit-sample-rate",
        });
    }
    let channel_config = b.read(4).ok_or_else(bad)? as u8;
    if aot == 5 || aot == 29 {
        return Err(ReelError::Unsupported {
            what: "aac-he-aac-sbr-ps",
        });
    }
    if aot != 2 {
        return Err(ReelError::Unsupported {
            what: "aac-profile-not-lc",
        });
    }
    if b.bit().ok_or_else(bad)? {
        return Err(ReelError::Unsupported {
            what: "aac-960-sample-frames",
        });
    }
    let sample_rate = SAMPLE_RATES
        .get(usize::from(sf_index))
        .copied()
        .ok_or_else(bad)?;
    if sample_rate > MAX_SAMPLE_RATE {
        return Err(ReelError::LimitExceeded {
            what: "aac-sample-rate",
        });
    }
    if channel_config == 0 {
        return Err(ReelError::Unsupported {
            what: "aac-program-config-element",
        });
    }
    let channels = config_channels(channel_config)
        .and_then(|c| u16::try_from(c).ok())
        .ok_or(ReelError::Unsupported {
            what: "aac-channel-configuration",
        })?;
    if channels > MAX_AUDIO_CHANNELS {
        return Err(ReelError::LimitExceeded {
            what: "aac-channels",
        });
    }
    Ok(AacConfig {
        object_type: aot as u8,
        sf_index,
        sample_rate,
        channel_config,
        channels,
    })
}

/// Channels of an AAC channel configuration (ISO/IEC 14496-3 channel configuration table: 1-6 channels
/// for configurations 1-6, eight for configuration 7).
fn config_channels(config: u8) -> Option<usize> {
    match config {
        1..=6 => Some(usize::from(config)),
        7 => Some(8),
        _ => None,
    }
}

/// Rate and channels of an AudioSpecificConfig (see `parse_audio_specific_config`).
pub fn check_audio_specific_config(asc: &[u8]) -> Result<(u32, u16), ReelError> {
    parse_audio_specific_config(asc).map(|c| (c.sample_rate, c.channels))
}

/// Studio's AAC-LC decoder as a `PacketDecoder` (cargo feature `studio-aac`).
#[cfg(feature = "studio-aac")]
pub(crate) struct AacPacketDecoder {
    decoder: AacLc,
    poisoned: bool,
}

#[cfg(feature = "studio-aac")]
impl AacPacketDecoder {
    pub(crate) fn new(asc: &[u8]) -> Result<Self, ReelError> {
        let config = parse_audio_specific_config(asc)?;
        Ok(Self {
            decoder: AacLc::new(config.sf_index, config.channel_config)?,
            poisoned: false,
        })
    }
}

#[cfg(feature = "studio-aac")]
impl PacketDecoder for AacPacketDecoder {
    fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, ReelError> {
        if packet.len() > MAX_ADTS_FRAME {
            return Err(ReelError::LimitExceeded {
                what: "aac-access-unit-size",
            });
        }
        let decoder = &mut self.decoder;
        let before = out.len();
        let frames = guarded(&mut self.poisoned, || decoder.decode(packet, out))?;
        if out.len().saturating_sub(before) != frames.saturating_mul(decoder.channels())
            || frames != FRAME
        {
            return Err(ReelError::Corrupt {
                what: "aac-access-unit",
            });
        }
        Ok(frames)
    }
}

/// A chosen AAC decoder and the `(decoder_id, decoder_version)` its receipts carry.
pub(crate) type ChosenDecoder = (Box<dyn PacketDecoder>, (&'static str, &'static str));

/// The AAC-LC decoder of a stream (D24-A order): the external decoder when one is registered and supports
/// AAC-LC, else Studio's decoder (feature `studio-aac`), else `MEDIA_CODEC_UNAVAILABLE aac-lc`. The
/// AudioSpecificConfig is parsed and checked here, before any decoder (or worker) starts. Returns the decoder
/// and the `(decoder_id, decoder_version)` its receipts carry.
pub(crate) fn aac_decoder(
    asc: &[u8],
    external: Option<&dyn ExternalAudioDecoder>,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<ChosenDecoder, ReelError> {
    let config = parse_audio_specific_config(asc)?;
    if let Some(decoder) = external.filter(|d| d.supports(AudioCodec::AacLc)) {
        let spec = AudioStreamSpec {
            codec: AudioCodec::AacLc,
            config: asc,
            sample_rate: config.sample_rate,
            channels: config.channels,
            channel_config: config.channel_config,
            limits: *limits,
        };
        let session = Checked::open(decoder, &spec, cancel)?;
        let identity = decoder.identity();
        return Ok((Box::new(Bounded(session)), (identity.id, identity.version)));
    }
    #[cfg(feature = "studio-aac")]
    {
        Ok((
            Box::new(AacPacketDecoder::new(asc)?),
            (AAC_DECODER_ID, AAC_DECODER_VERSION),
        ))
    }
    #[cfg(not(feature = "studio-aac"))]
    {
        Err(ReelError::CodecUnavailable {
            codec: AudioCodec::AacLc.as_str().to_owned(),
        })
    }
}

/// The access-unit size bound in front of an external session (the same bound Studio's decoder applies).
struct Bounded(Checked);

impl PacketDecoder for Bounded {
    fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, ReelError> {
        if packet.len() > MAX_ADTS_FRAME {
            return Err(ReelError::LimitExceeded {
                what: "aac-access-unit-size",
            });
        }
        self.0.decode(packet, out)
    }
    fn flush(&mut self, out: &mut Vec<f32>) -> Result<usize, ReelError> {
        self.0.flush(out)
    }
}

// ----- ADTS elementary streams -------------------------------------------------------------------------

pub(crate) struct AdtsInfo {
    pub(crate) first: AdtsHeader,
    pub(crate) offset: u64,
    pub(crate) shape: StreamShape,
}

fn read_at(src: &mut dyn ReadSeek, at: u64, buf: &mut [u8]) -> Result<usize, ReelError> {
    src.seek(SeekFrom::Start(at))?;
    let mut got = 0usize;
    while got < buf.len() {
        let n = src.read(buf.get_mut(got..).unwrap_or(&mut []))?;
        if n == 0 {
            break;
        }
        got = got.saturating_add(n);
    }
    Ok(got)
}

/// First header and the total from a header walk (each frame carries 1024 samples per channel).
pub(crate) fn probe_adts(
    src: &mut dyn ReadSeek,
    file_len: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<AdtsInfo, ReelError> {
    let mut head = [0u8; 9];
    let n = read_at(src, 0, &mut head)?;
    let first =
        AdtsHeader::parse(head.get(..n).unwrap_or(&[])).ok_or(ReelError::ImportUnsupported {
            detected: "not-adts".into(),
        })?;
    if first.raw_blocks != 0 {
        return Err(ReelError::Unsupported {
            what: "adts-multiple-raw-data-blocks",
        });
    }
    let (rate, channels) = check_audio_specific_config(&first.audio_specific_config())?;
    let mut at = 0u64;
    let mut frames = 0u64;
    loop {
        if frames.is_multiple_of(4096) {
            check_cancel(cancel)?;
        }
        let mut h = [0u8; 9];
        let got = read_at(src, at, &mut h)?;
        let Some(header) = AdtsHeader::parse(h.get(..got).unwrap_or(&[])) else {
            break;
        };
        if !header.continues(&first) || at.saturating_add(header.frame_len as u64) > file_len {
            break;
        }
        frames = frames.saturating_add(1);
        if frames > limits.max_samples as u64 {
            return Err(ReelError::LimitExceeded {
                what: "adts-frames",
            });
        }
        at = at.saturating_add(header.frame_len as u64);
    }
    Ok(AdtsInfo {
        first,
        offset: 0,
        shape: StreamShape {
            rate,
            channels,
            total: Some(frames.saturating_mul(FRAME as u64)),
            skip: 0,
        },
    })
}

pub(crate) struct AdtsStream<'a> {
    src: &'a mut dyn ReadSeek,
    first: AdtsHeader,
    at: u64,
    bytes_read: u64,
    decoder: Box<dyn PacketDecoder>,
    frame: Vec<u8>,
}

impl<'a> AdtsStream<'a> {
    /// `decoder` comes from `aac_decoder` over the first header's AudioSpecificConfig.
    pub(crate) fn open(
        src: &'a mut dyn ReadSeek,
        info: &AdtsInfo,
        decoder: Box<dyn PacketDecoder>,
    ) -> Self {
        Self {
            src,
            first: info.first,
            at: info.offset,
            bytes_read: 0,
            decoder,
            frame: Vec::with_capacity(MAX_ADTS_FRAME),
        }
    }

    /// End of the access units: what the decoder still holds, else the end of the stream.
    fn end(&mut self, out: &mut Vec<f32>) -> Result<Option<usize>, ReelError> {
        match self.decoder.flush(out)? {
            0 => Ok(None),
            frames => Ok(Some(frames)),
        }
    }
}

impl FrameSource for AdtsStream<'_> {
    fn next_block(
        &mut self,
        out: &mut Vec<f32>,
        cancel: &CancellationToken,
    ) -> Result<Option<usize>, ReelError> {
        check_cancel(cancel)?;
        let mut h = [0u8; 9];
        let got = read_at(self.src, self.at, &mut h)?;
        let Some(header) = AdtsHeader::parse(h.get(..got).unwrap_or(&[])) else {
            return self.end(out);
        };
        if !header.continues(&self.first) {
            return self.end(out);
        }
        if header.raw_blocks != 0 {
            return Err(ReelError::Unsupported {
                what: "adts-multiple-raw-data-blocks",
            });
        }
        self.frame.resize(header.frame_len, 0);
        if read_at(self.src, self.at, &mut self.frame)? < header.frame_len {
            return Err(ReelError::Truncated { delivered: 0 });
        }
        self.at = self.at.saturating_add(header.frame_len as u64);
        self.bytes_read = self.bytes_read.saturating_add(header.frame_len as u64);
        let payload = self.frame.get(header.header_len..).unwrap_or(&[]);
        self.decoder.decode(payload, out).map(Some)
    }

    fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}

/// AudioSpecificConfig from an MP4 `esds` box payload (ISO/IEC 14496-1 ES_Descriptor ->
/// DecoderConfigDescriptor (tag 4) -> DecoderSpecificInfo (tag 5)), with the descriptor's
/// objectTypeIndication. Input starts with the four FullBox version/flags bytes, excluding
/// the box size and `esds` type. Descriptors traversed for configuration extraction are bounded
/// by their declared parents, and their variable-length sizes must terminate within four bytes.
/// Unrelated trailing siblings are not validated. A missing DecoderSpecificInfo returns an
/// empty configuration (for example MPEG audio); this is not full-container or decode validation.
pub fn asc_from_esds(esds: &[u8]) -> Result<(u8, Vec<u8>), ReelError> {
    fn descriptor(data: &[u8]) -> Result<(u8, &[u8]), ReelError> {
        let bad = || ReelError::Corrupt { what: "mp4-esds" };
        let tag = *data.first().ok_or_else(bad)?;
        let mut len = 0usize;
        for at in 1usize..=4 {
            let b = *data.get(at).ok_or_else(bad)?;
            len = (len << 7) | usize::from(b & 0x7F);
            if b & 0x80 == 0 {
                let start = at.saturating_add(1);
                let end = start.checked_add(len).ok_or_else(bad)?;
                return Ok((tag, data.get(start..end).ok_or_else(bad)?));
            }
        }
        Err(bad())
    }
    let bad = || ReelError::Corrupt { what: "mp4-esds" };
    let (tag, es) = descriptor(esds.get(4..).ok_or_else(bad)?)?;
    if tag != 3 {
        return Err(bad());
    }
    let flags = *es.get(2).ok_or_else(bad)?;
    let mut rest = es.get(3..).ok_or_else(bad)?;
    if flags & 0x80 != 0 {
        rest = rest.get(2..).ok_or_else(bad)?;
    }
    if flags & 0x40 != 0 {
        let url_len = usize::from(*rest.first().ok_or_else(bad)?);
        rest = rest.get(url_len.saturating_add(1)..).ok_or_else(bad)?;
    }
    if flags & 0x20 != 0 {
        rest = rest.get(2..).ok_or_else(bad)?;
    }
    let (tag, decoder) = descriptor(rest)?;
    if tag != 4 {
        return Err(bad());
    }
    let oti = *decoder.first().ok_or_else(bad)?;
    let rest = decoder.get(13..).ok_or_else(bad)?;
    if rest.is_empty() {
        return Ok((oti, Vec::new()));
    }
    let (tag, asc) = descriptor(rest)?;
    if tag != 5 {
        // No DecoderSpecificInfo (MP3 in MP4 carries none).
        return Ok((oti, Vec::new()));
    }
    Ok((oti, asc.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adts_header_round_trips_to_audio_specific_config() {
        // AAC-LC (profile 1), 44.1 kHz (index 4), stereo, frame 0x173 bytes, no CRC.
        let h = [0xFF, 0xF1, 0x50, 0x80, 0x2E, 0x7F, 0xFC];
        let a = AdtsHeader::parse(&h).unwrap();
        assert_eq!(
            (
                a.object_type,
                SAMPLE_RATES[usize::from(a.sf_index)],
                a.channel_config,
                a.frame_len
            ),
            (2, 44_100, 2, 0x173)
        );
        assert_eq!(a.audio_specific_config(), [0x12, 0x10]);
        assert_eq!(
            check_audio_specific_config(&a.audio_specific_config()).unwrap(),
            (44_100, 2)
        );
    }
}
