//! Compressed audio codecs for Audition and Premiere media (lane SMB-AUDIO-CODECS; STU-VID-050 import
//! surface, STU-VID-051 adapter contract as enriched by D23).
//!
//! Every codec here is a declared adapter with a `CodecDescriptor` (decoder id, version, licence,
//! certification), enumerable through `audio_codec_descriptors` / `Registry::audio_codec_descriptors`;
//! a codec without an adapter stays the typed `MEDIA_CODEC_UNAVAILABLE`.
//!
//! Decoded output, whatever the codec: `ChunkKind::AudioSamples` chunks of INTERLEAVED little-endian
//! IEEE f32 samples, nominally in [-1, 1] (integer PCM is scaled by 2^-(bits-1)), at the stream's own
//! sample rate. Hostile input: every header field that sizes an allocation (sample rate, channels, block
//! size, packet size) is checked against the bounds below before anything is allocated (V4-01/V4-02).
#![deny(clippy::arithmetic_side_effects)]

mod bits;
pub(crate) mod engine;
pub mod flac;
mod md5;
pub mod mp3;
mod mp3_dsp;
mod mp3_stream;
mod native_flac;
mod ogg;
mod ogg_audio;
pub mod opus;
mod packet;
mod provider;
pub mod vorbis;

use crate::error::ReelError;
use crate::model::{
    Certification, CodecDescriptor, Limits, MediaKind, ReadSeek, STUDIO_OWNED_LICENCE,
};
use hsk_studio_accord::CancellationToken;

pub use mp3::{MP3_DECODER_ID, MP3_DECODER_VERSION, OUTPUT_BOUND as MP3_OUTPUT_BOUND};
pub use ogg_audio::OggCodec;
pub use opus::{OPUS_DECODER_ID, OPUS_DECODER_VERSION};
pub use provider::{
    AudioFileKind, AudioFileProvider, FLAC_PROVIDER_ID, MP3_PROVIDER_ID, OGG_PROVIDER_ID,
};
pub use vorbis::{VORBIS_DECODER_ID, VORBIS_DECODER_VERSION};

/// Highest sample rate any audio header may declare (768 kHz, the highest rate of common DAW hardware);
/// above it a header is refused as `MEDIA_LIMIT_EXCEEDED` before allocation.
pub const MAX_SAMPLE_RATE: u32 = 768_000;
/// Most channels a decoded audio stream may have (7.1).
pub const MAX_AUDIO_CHANNELS: u16 = 8;

/// FLAC decoder id (receipts carry it as `decoder_id`).
pub const FLAC_DECODER_ID: &str = "flac-rfc9639";
/// Bumped whenever any output bit of the FLAC decoder can change.
pub const FLAC_DECODER_VERSION: &str = "1";

/// Descriptor of the FLAC adapter (decode and encode, Studio-owned, written from RFC 9639).
pub fn flac_descriptor() -> CodecDescriptor {
    CodecDescriptor {
        id: "flac",
        name: "FLAC (RFC 9639)",
        kind: MediaKind::Audio,
        container: "flac|ogg|isobmff|matroska",
        bit_depths: &[8, 12, 16, 20, 24, 32],
        chroma: None,
        transfer: None,
        decoder_id: FLAC_DECODER_ID,
        decoder_version: FLAC_DECODER_VERSION,
        licence: STUDIO_OWNED_LICENCE,
        certification: Certification::NotApplicable,
        can_decode: true,
        can_encode: true,
    }
}

/// Descriptor of the Vorbis adapter (decode through lewton 0.10.2).
pub fn vorbis_descriptor() -> CodecDescriptor {
    CodecDescriptor {
        id: "vorbis",
        name: "Vorbis I (lewton decoder)",
        kind: MediaKind::Audio,
        container: "ogg|matroska",
        bit_depths: &[32],
        chroma: None,
        transfer: None,
        decoder_id: VORBIS_DECODER_ID,
        decoder_version: VORBIS_DECODER_VERSION,
        licence: "MIT OR Apache-2.0",
        certification: Certification::NotApplicable,
        can_decode: true,
        can_encode: false,
    }
}

/// Descriptor of the Opus adapter (decode through opus-pure 0.2.2, 48 kHz output).
pub fn opus_descriptor() -> CodecDescriptor {
    CodecDescriptor {
        id: "opus",
        name: "Opus (opus-pure decoder, RFC 6716)",
        kind: MediaKind::Audio,
        container: "ogg|matroska|isobmff",
        bit_depths: &[16],
        chroma: None,
        transfer: None,
        decoder_id: OPUS_DECODER_ID,
        decoder_version: OPUS_DECODER_VERSION,
        licence: "BSD-3-Clause",
        certification: Certification::NotApplicable,
        can_decode: true,
        can_encode: false,
    }
}

/// Descriptor of the MPEG audio adapter (Layers I, II and III; Studio-owned port of minimp3, CC0).
pub fn mp3_descriptor() -> CodecDescriptor {
    CodecDescriptor {
        id: "mp3",
        name: "MPEG-1/2/2.5 Audio Layers I-III (minimp3 port)",
        kind: MediaKind::Audio,
        container: "mpeg-audio|isobmff|matroska",
        bit_depths: &[32],
        chroma: None,
        transfer: None,
        decoder_id: MP3_DECODER_ID,
        decoder_version: MP3_DECODER_VERSION,
        licence: STUDIO_OWNED_LICENCE,
        certification: Certification::NotCertified,
        can_decode: true,
        can_encode: false,
    }
}

/// The audio codec adapters of this build, in a stable order (STU-VID-051 item 2).
pub fn audio_codec_descriptors() -> Vec<CodecDescriptor> {
    vec![
        flac_descriptor(),
        vorbis_descriptor(),
        opus_descriptor(),
        mp3_descriptor(),
    ]
}

/// A whole decoded audio stream (for import into Score, which works on planar f32 blocks).
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAudio {
    pub sample_rate: u32,
    pub channels: u16,
    /// Interleaved samples, nominally in [-1, 1].
    pub interleaved: Vec<f32>,
    pub decoder_id: &'static str,
    pub decoder_version: &'static str,
    /// Lossless codecs that carry a checksum of the decoded audio (FLAC MD5): whether it matched;
    /// `None` when the stream declares none.
    pub checksum_verified: Option<bool>,
}

impl DecodedAudio {
    /// Samples per channel.
    pub fn frames(&self) -> usize {
        self.interleaved
            .len()
            .checked_div(usize::from(self.channels))
            .unwrap_or(0)
    }

    /// One `Vec<f32>` per channel (Score's planar block layout).
    pub fn planar(&self) -> Vec<Vec<f32>> {
        let channels = usize::from(self.channels);
        let mut out = vec![Vec::with_capacity(self.frames()); channels];
        for frame in self.interleaved.chunks(channels.max(1)) {
            for (dst, v) in out.iter_mut().zip(frame) {
                dst.push(*v);
            }
        }
        out
    }
}

/// Decode a whole standalone audio file (format detected from its first bytes) into memory, refusing
/// before allocation when it would hold more than `max_frames` samples per channel.
pub fn decode_audio_file(
    src: &mut dyn ReadSeek,
    limits: &Limits,
    max_frames: u64,
    cancel: &CancellationToken,
) -> Result<DecodedAudio, ReelError> {
    provider::decode_file(src, limits, max_frames, cancel)
}

/// Decode a native FLAC stream and also return its integer samples (bit-exact checks, conversions).
pub fn decode_flac_integers(
    src: &mut dyn ReadSeek,
    limits: &Limits,
    max_frames: u64,
    cancel: &CancellationToken,
) -> Result<(flac::StreamInfo, Vec<i32>, Option<bool>), ReelError> {
    provider::decode_flac_integers(src, limits, max_frames, cancel)
}

/// Encode interleaved integer samples into a complete native FLAC stream with a full STREAMINFO (MD5 and
/// frame sizes), for callers that hold the samples (the streaming `encode_segment` port writes the
/// header before the audio and therefore leaves those fields unknown, which RFC 9639 allows).
pub fn encode_flac(
    sample_rate: u32,
    channels: u8,
    bits_per_sample: u8,
    interleaved: &[i32],
) -> Result<Vec<u8>, ReelError> {
    let mut encoder = flac::FlacEncoder::new(sample_rate, channels, bits_per_sample)?;
    let mut frames = Vec::new();
    encoder.push(interleaved, &mut frames)?;
    let info = encoder.finish(&mut frames)?;
    let mut out = flac::header_bytes(&info);
    out.extend_from_slice(&frames);
    Ok(out)
}
