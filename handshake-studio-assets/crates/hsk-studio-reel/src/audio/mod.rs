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

pub mod aac;
#[cfg(feature = "studio-aac")]
mod aac_dsp;
#[cfg(feature = "studio-aac")]
mod aac_lc;
pub mod aiff;
mod bits;
pub(crate) mod container;
pub(crate) mod engine;
pub mod external;
pub mod flac;
mod md5;
pub mod speakers;
mod wav_file;
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

pub use aac::{AAC_DECODER_ID, AAC_DECODER_VERSION};
pub use external::{
    AAC_OS_DECODER_ID, AAC_OS_DISPLAY_NAME, AAC_OS_LICENCE, AudioCodec, AudioDecodeSession,
    AudioStreamSpec, EXTERNAL_OUTPUT_BOUND, ExternalAudioDecoder, FRAMES_PER_ACCESS_UNIT,
    MAX_EXTERNAL_BLOCK_FRAMES, aac_os_descriptor,
};
pub use mp3::{MP3_DECODER_ID, MP3_DECODER_VERSION, OUTPUT_BOUND as MP3_OUTPUT_BOUND};
pub use ogg_audio::OggCodec;
pub use opus::{OPUS_DECODER_ID, OPUS_DECODER_VERSION};
pub use provider::{
    AAC_PROVIDER_ID, AIFF_PROVIDER_ID, AudioFileKind, AudioFileProvider, FLAC_PROVIDER_ID,
    MP3_PROVIDER_ID, OGG_PROVIDER_ID, sniff_audio_file,
};
pub use md5::Md5;
pub use speakers::Speaker;
pub use vorbis::{VORBIS_DECODER_ID, VORBIS_DECODER_VERSION};
pub use wav_file::WAV_FILE_DECODER_ID;

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

/// Descriptor of Studio's own AAC-LC adapter (channel configurations 1-7). Opt-in since Operator decision
/// D24-A: `can_decode` only with cargo feature `studio-aac`; by default AAC decodes through the OS decoder
/// (`aac_os_descriptor`, reel-native's worker).
pub fn aac_descriptor() -> CodecDescriptor {
    CodecDescriptor {
        id: "aac-lc",
        name: "AAC-LC (ISO/IEC 14496-3 decoder, channel configurations 1-7)",
        kind: MediaKind::Audio,
        container: "adts|isobmff|matroska",
        bit_depths: &[32],
        chroma: None,
        transfer: None,
        decoder_id: AAC_DECODER_ID,
        decoder_version: AAC_DECODER_VERSION,
        licence: STUDIO_OWNED_LICENCE,
        certification: Certification::NotCertified,
        can_decode: cfg!(feature = "studio-aac"),
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
        aac_descriptor(),
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
    /// Loudspeaker of each channel in delivery order, from the codec's channel-order rule or the file's
    /// channel mask (`speakers::layout_for`); `None` when the format defines no order for this count.
    pub speakers: Option<Vec<Speaker>>,
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
    provider::decode_file(src, limits, max_frames, cancel, None)
}

/// `decode_audio_file` that also refuses before allocation when the decoded audio would hold more than
/// `max_samples` interleaved samples (frames x channels; R47 F-R47-1): a declared total over it is
/// refused before any payload is read, and before each decoded block is kept the kept samples plus the
/// block's are checked, so the returned buffer never holds, nor reserves, more than `max_samples` f32
/// samples. Over it: `ReelError::LimitExceeded { what: "audio-sample-budget" }`. `max_frames` keeps its
/// meaning (samples per channel). Test: `tests/audio_sample_budget.rs`.
pub fn decode_audio_file_within(
    src: &mut dyn ReadSeek,
    limits: &Limits,
    max_frames: u64,
    max_samples: u64,
    cancel: &CancellationToken,
) -> Result<DecodedAudio, ReelError> {
    provider::decode_file_within(src, limits, max_frames, Some(max_samples), cancel, None)
}

/// `decode_audio_file` with an external audio decoder for the codecs it supports (D24-A: AAC through the
/// OS decoder in reel-native's worker); `None` is `decode_audio_file`.
pub fn decode_audio_file_with(
    src: &mut dyn ReadSeek,
    limits: &Limits,
    max_frames: u64,
    cancel: &CancellationToken,
    external: Option<&dyn ExternalAudioDecoder>,
) -> Result<DecodedAudio, ReelError> {
    provider::decode_file(src, limits, max_frames, cancel, external)
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
