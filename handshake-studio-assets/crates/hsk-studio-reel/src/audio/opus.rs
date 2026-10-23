//! Opus decode (RFC 6716, Ogg mapping RFC 7845) through `opus-pure` 0.2.2 (BSD-3-Clause, no
//! dependencies, no C; a Rust port of libopus that documents bit-exact SILK and float-rounding CELT
//! agreement with libopus 1.6.1 and a fuzzed robustness sweep; its SIMD kernels use `unsafe` with scalar
//! fallbacks). Reel parses the identification header (`OpusHead`, RFC 7845 section 5.1, or the ISO BMFF
//! `dOps` box) itself, bounds channels and streams before any decoder exists, splits multistream
//! packets itself (RFC 6716 Appendix B), applies the output gain, and runs every decoder call behind
//! `packet::guarded`. Output is always 48 kHz.
#![deny(clippy::arithmetic_side_effects)]
use super::MAX_AUDIO_CHANNELS;
use super::packet::{PacketDecoder, guarded};
use crate::error::ReelError;
use opus_pure::OpusDecoder;

/// Opus decoder id (receipts carry it as `decoder_id`).
pub const OPUS_DECODER_ID: &str = "opus-pure-0.2.2";
pub const OPUS_DECODER_VERSION: &str = "1";

/// Opus decodes at 48 kHz (RFC 7845 section 5.1: the input rate is informational).
pub const OPUS_RATE: u32 = 48_000;
/// Largest packet: 120 ms of 1275-byte frames per stream (RFC 6716 section 3.2.5) with room for
/// multistream self-delimiting framing.
pub(crate) const MAX_OPUS_PACKET: usize = 64 * 1024;
/// 120 ms at 48 kHz: the most samples per channel one packet decodes to.
const MAX_FRAMES_PER_PACKET: usize = 5_760;

/// Identification header fields (RFC 7845 section 5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpusHead {
    pub channels: u8,
    pub pre_skip: u16,
    pub input_rate: u32,
    /// Q7.8 dB.
    pub output_gain: i16,
    pub mapping_family: u8,
    pub streams: u8,
    pub coupled: u8,
    pub mapping: Vec<u8>,
}

const CORRUPT_HEAD: ReelError = ReelError::Corrupt { what: "opus-head" };

impl OpusHead {
    /// Ogg / Matroska form: "OpusHead", little-endian fields.
    pub fn parse(packet: &[u8]) -> Result<Self, ReelError> {
        if packet.get(..8) != Some(b"OpusHead") {
            return Err(CORRUPT_HEAD);
        }
        let version = *packet.get(8).ok_or(CORRUPT_HEAD)?;
        if version >> 4 != 0 {
            return Err(ReelError::Unsupported {
                what: "opus-head-version",
            });
        }
        let le16 = |at: usize| -> Result<u16, ReelError> {
            Ok(u16::from_le_bytes([
                *packet.get(at).ok_or(CORRUPT_HEAD)?,
                *packet.get(at.saturating_add(1)).ok_or(CORRUPT_HEAD)?,
            ]))
        };
        let channels = *packet.get(9).ok_or(CORRUPT_HEAD)?;
        let pre_skip = le16(10)?;
        let input_rate = u32::from(le16(12)?) | (u32::from(le16(14)?) << 16);
        let output_gain = le16(16)? as i16;
        let mapping_family = *packet.get(18).ok_or(CORRUPT_HEAD)?;
        Self::finish(
            packet.get(19..).unwrap_or(&[]),
            channels,
            pre_skip,
            input_rate,
            output_gain,
            mapping_family,
        )
    }

    /// ISO BMFF `dOps` box payload ("Encapsulation of Opus in ISO Base Media File Format" section 4.3.2):
    /// version 0, big-endian fields, same channel mapping table.
    pub fn parse_dops(payload: &[u8]) -> Result<Self, ReelError> {
        if payload.first() != Some(&0) {
            return Err(ReelError::Unsupported {
                what: "opus-dops-version",
            });
        }
        let be16 = |at: usize| -> Result<u16, ReelError> {
            Ok(u16::from_be_bytes([
                *payload.get(at).ok_or(CORRUPT_HEAD)?,
                *payload.get(at.saturating_add(1)).ok_or(CORRUPT_HEAD)?,
            ]))
        };
        let channels = *payload.get(1).ok_or(CORRUPT_HEAD)?;
        let pre_skip = be16(2)?;
        let input_rate = (u32::from(be16(4)?) << 16) | u32::from(be16(6)?);
        let output_gain = be16(8)? as i16;
        let mapping_family = *payload.get(10).ok_or(CORRUPT_HEAD)?;
        Self::finish(
            payload.get(11..).unwrap_or(&[]),
            channels,
            pre_skip,
            input_rate,
            output_gain,
            mapping_family,
        )
    }

    fn finish(
        table: &[u8],
        channels: u8,
        pre_skip: u16,
        input_rate: u32,
        output_gain: i16,
        mapping_family: u8,
    ) -> Result<Self, ReelError> {
        if channels == 0 {
            return Err(CORRUPT_HEAD);
        }
        if u16::from(channels) > MAX_AUDIO_CHANNELS {
            return Err(ReelError::LimitExceeded {
                what: "opus-channels",
            });
        }
        let (streams, coupled, mapping) = if mapping_family == 0 {
            if channels > 2 {
                return Err(CORRUPT_HEAD);
            }
            (1, channels.saturating_sub(1), (0..channels).collect())
        } else {
            let streams = *table.first().ok_or(CORRUPT_HEAD)?;
            let coupled = *table.get(1).ok_or(CORRUPT_HEAD)?;
            let mapping = table
                .get(2..2usize.saturating_add(usize::from(channels)))
                .ok_or(CORRUPT_HEAD)?
                .to_vec();
            if streams == 0 || coupled > streams {
                return Err(CORRUPT_HEAD);
            }
            let decoded = u16::from(streams).saturating_add(u16::from(coupled));
            if decoded > 255
                || mapping
                    .iter()
                    .any(|m| *m != 255 && u16::from(*m) >= decoded)
            {
                return Err(CORRUPT_HEAD);
            }
            if decoded > MAX_AUDIO_CHANNELS.saturating_mul(2) {
                return Err(ReelError::LimitExceeded {
                    what: "opus-streams",
                });
            }
            (streams, coupled, mapping)
        };
        Ok(Self {
            channels,
            pre_skip,
            input_rate,
            output_gain,
            mapping_family,
            streams,
            coupled,
            mapping,
        })
    }
}

/// Elementary streams of a multistream packet (RFC 7845 section 5.1.1, RFC 6716 Appendix B), each
/// decoded by its own single-stream decoder, so the header's own channel mapping table is honoured
/// (the decoding crate's multistream type derives the table from the mapping family). History: the
/// first candidate crate, opus-decoder 0.1.1, split self-delimited packets wrongly (length prefix read
/// before the TOC byte, RFC 6716 Appendix B puts it after) and panicked on a shift overflow in CELT
/// (`celt/vq.rs:118`, collapse mask `1u8 << i`) on a libopus 5.1 file; it was replaced.
struct Streams {
    decoders: Vec<OpusDecoder>,
    coupled: usize,
    mapping: Vec<u8>,
    buffers: Vec<Vec<f32>>,
}

fn self_delimited_error() -> ReelError {
    ReelError::Corrupt {
        what: "opus-self-delimited",
    }
}

/// One frame length (RFC 6716 section 3.2.1): one byte below 252, else two bytes (first + 4 * second).
fn frame_length(data: &[u8], at: usize) -> Result<(usize, usize), ReelError> {
    let first = usize::from(*data.get(at).ok_or_else(self_delimited_error)?);
    if first < 252 {
        return Ok((first, 1));
    }
    let second = usize::from(
        *data
            .get(at.saturating_add(1))
            .ok_or_else(self_delimited_error)?,
    );
    Ok((first.saturating_add(second.saturating_mul(4)), 2))
}

fn slice_at(data: &[u8], at: usize, len: usize) -> Result<&[u8], ReelError> {
    data.get(at..at.checked_add(len).ok_or_else(self_delimited_error)?)
        .ok_or_else(self_delimited_error)
}

/// Split one self-delimited elementary packet off the front of `data` (RFC 6716 Appendix B) and rewrite
/// it in the undelimited form a single-stream decoder takes. Returns (packet, bytes consumed).
fn take_self_delimited(data: &[u8]) -> Result<(Vec<u8>, usize), ReelError> {
    let toc = *data.first().ok_or_else(self_delimited_error)?;
    let mut out = vec![toc];
    let mut at = 1usize;
    let end = match toc & 3 {
        0 => {
            let (last, n) = frame_length(data, at)?;
            at = at.saturating_add(n);
            out.extend_from_slice(slice_at(data, at, last)?);
            at.saturating_add(last)
        }
        1 => {
            let (last, n) = frame_length(data, at)?;
            at = at.saturating_add(n);
            let both = last.checked_mul(2).ok_or_else(self_delimited_error)?;
            out.extend_from_slice(slice_at(data, at, both)?);
            at.saturating_add(both)
        }
        2 => {
            let (first, n1) = frame_length(data, at)?;
            out.extend_from_slice(slice_at(data, at, n1)?);
            at = at.saturating_add(n1);
            let (last, n) = frame_length(data, at)?;
            at = at.saturating_add(n);
            let both = first.checked_add(last).ok_or_else(self_delimited_error)?;
            out.extend_from_slice(slice_at(data, at, both)?);
            at.saturating_add(both)
        }
        _ => {
            let count_byte = *data.get(at).ok_or_else(self_delimited_error)?;
            out.push(count_byte);
            at = at.saturating_add(1);
            let frames = usize::from(count_byte & 0x3F);
            let vbr = count_byte & 0x80 != 0;
            let mut padding = 0usize;
            if count_byte & 0x40 != 0 {
                loop {
                    let b = *data.get(at).ok_or_else(self_delimited_error)?;
                    out.push(b);
                    at = at.saturating_add(1);
                    padding = padding.saturating_add(if b == 255 { 254 } else { usize::from(b) });
                    if b != 255 {
                        break;
                    }
                }
            }
            let (last, n) = frame_length(data, at)?;
            at = at.saturating_add(n);
            let mut total = 0usize;
            if vbr {
                for _ in 1..frames {
                    let (len, n) = frame_length(data, at)?;
                    out.extend_from_slice(slice_at(data, at, n)?);
                    at = at.saturating_add(n);
                    total = total.checked_add(len).ok_or_else(self_delimited_error)?;
                }
                total = total.checked_add(last).ok_or_else(self_delimited_error)?;
            } else {
                total = last.checked_mul(frames).ok_or_else(self_delimited_error)?;
            }
            let with_padding = total
                .checked_add(padding)
                .ok_or_else(self_delimited_error)?;
            out.extend_from_slice(slice_at(data, at, with_padding)?);
            at.saturating_add(with_padding)
        }
    };
    Ok((out, end))
}

impl Streams {
    /// Decode one multistream packet into `scratch` (interleaved, `channels` wide). Returns frames.
    fn decode(
        &mut self,
        packet: &[u8],
        scratch: &mut [f32],
        channels: usize,
    ) -> Result<usize, ReelError> {
        let streams = self.decoders.len();
        let mut rest = packet;
        let mut frames: Option<usize> = None;
        let streams_error = || ReelError::Corrupt {
            what: "opus-streams",
        };
        for index in 0..streams {
            let sub = if index.saturating_add(1) < streams {
                let (sub, used) = take_self_delimited(rest)?;
                rest = rest.get(used..).unwrap_or(&[]);
                sub
            } else {
                rest.to_vec()
            };
            let width = if index < self.coupled { 2 } else { 1 };
            let decoder = self.decoders.get_mut(index).ok_or_else(streams_error)?;
            let buffer = self.buffers.get_mut(index).ok_or_else(streams_error)?;
            let n = decoder
                .decode(&sub, MAX_FRAMES_PER_PACKET, buffer)
                .map_err(|_| ReelError::Corrupt {
                    what: "opus-packet",
                })?;
            if frames.is_some_and(|f| f != n) {
                return Err(ReelError::Corrupt {
                    what: "opus-stream-durations",
                });
            }
            frames = Some(n);
            if buffer.len() < n.saturating_mul(width) {
                return Err(ReelError::Corrupt {
                    what: "opus-packet",
                });
            }
        }
        let frames = frames.unwrap_or(0);
        let coupled_slots = self.coupled.saturating_mul(2);
        for (ch, &slot) in self.mapping.iter().enumerate() {
            let slot = usize::from(slot);
            // (stream, width, channel within the stream); 255 = silence (RFC 7845 section 5.1.1).
            let source = if slot == 255 {
                None
            } else if slot < coupled_slots {
                Some((slot >> 1, 2usize, slot & 1))
            } else {
                Some((
                    self.coupled
                        .saturating_add(slot.saturating_sub(coupled_slots)),
                    1,
                    0,
                ))
            };
            for i in 0..frames {
                let v = source.map_or(0.0, |(stream, width, offset)| {
                    self.buffers
                        .get(stream)
                        .and_then(|b| b.get(i.saturating_mul(width).saturating_add(offset)))
                        .copied()
                        .unwrap_or(0.0)
                });
                if let Some(dst) = scratch.get_mut(i.saturating_mul(channels).saturating_add(ch)) {
                    *dst = v;
                }
            }
        }
        Ok(frames)
    }
}

enum Inner {
    Single(Box<OpusDecoder>),
    Multi(Streams),
}

pub(crate) struct OpusPacketDecoder {
    inner: Inner,
    channels: usize,
    gain: f32,
    scratch: Vec<f32>,
    poisoned: bool,
}

impl OpusPacketDecoder {
    pub(crate) fn new(head: &OpusHead) -> Result<Self, ReelError> {
        let channels = usize::from(head.channels);
        let mut poisoned = false;
        let inner = guarded(&mut poisoned, || {
            let unsupported = |_| ReelError::Unsupported {
                what: "opus-channel-mapping",
            };
            if head.mapping_family == 0 {
                OpusDecoder::new(OPUS_RATE as i32, channels)
                    .map(|d| Inner::Single(Box::new(d)))
                    .map_err(unsupported)
            } else {
                let (streams, coupled) = (usize::from(head.streams), usize::from(head.coupled));
                let mut decoders = Vec::with_capacity(streams);
                let mut buffers = Vec::with_capacity(streams);
                for index in 0..streams {
                    let width = if index < coupled { 2 } else { 1 };
                    decoders.push(OpusDecoder::new(OPUS_RATE as i32, width).map_err(unsupported)?);
                    buffers.push(vec![0.0f32; MAX_FRAMES_PER_PACKET.saturating_mul(width)]);
                }
                Ok(Inner::Multi(Streams {
                    decoders,
                    coupled,
                    mapping: head.mapping.clone(),
                    buffers,
                }))
            }
        })?;
        // RFC 7845 section 5.1: gain = 10^(output_gain / (20 * 256)).
        let gain = 10f64.powf(f64::from(head.output_gain) / 5120.0) as f32;
        Ok(Self {
            inner,
            channels,
            gain,
            scratch: vec![0.0; MAX_FRAMES_PER_PACKET.saturating_mul(channels)],
            poisoned,
        })
    }
}

impl PacketDecoder for OpusPacketDecoder {
    fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, ReelError> {
        if packet.is_empty() {
            // An empty packet would be packet-loss concealment, not media.
            return Err(ReelError::Corrupt {
                what: "opus-empty-packet",
            });
        }
        if packet.len() > MAX_OPUS_PACKET {
            return Err(ReelError::LimitExceeded {
                what: "opus-packet-size",
            });
        }
        let (inner, scratch, channels) = (&mut self.inner, &mut self.scratch, self.channels);
        let frames = guarded(&mut self.poisoned, || match inner {
            Inner::Single(d) => d
                .decode(packet, MAX_FRAMES_PER_PACKET, scratch)
                .map_err(|_| ReelError::Corrupt {
                    what: "opus-packet",
                }),
            Inner::Multi(m) => m.decode(packet, scratch, channels),
        })?;
        let values = frames
            .checked_mul(self.channels)
            .filter(|v| *v <= self.scratch.len())
            .ok_or(ReelError::Corrupt {
                what: "opus-packet",
            })?;
        out.try_reserve(values)
            .map_err(|_| ReelError::LimitExceeded {
                what: "audio-alloc",
            })?;
        let gain = self.gain;
        out.extend(self.scratch.iter().take(values).map(|v| v * gain));
        Ok(frames)
    }
}
