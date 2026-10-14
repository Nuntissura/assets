//! Studio-owned video codec adapters (STU-VID-051 as enriched by D23; ship list STU-VID-084 / D24):
//! the always-compiled facts about them (descriptors, sample-entry codes, decoder identities), so a build
//! without a decoder still enumerates the adapter and answers with the typed `MEDIA_CODEC_UNAVAILABLE`.
//!
//! * ProRes: decoder written from SMPTE RDD 36:2022, compiled in only with cargo feature `prores`
//!   (default OFF, D24); user-facing name exactly `PRORES_DISPLAY_NAME`; never certified by Apple.
//! * DNx / VC-3: decoder written from SMPTE ST 2019-1:2016 (+ Amd 1:2022), compiled in every build (D24).
#![deny(clippy::arithmetic_side_effects)]
use crate::model::{Certification, CodecDescriptor, MediaKind, STUDIO_OWNED_LICENCE};
use crate::video::{DecoderIdentity, VideoCodec};

/// User-facing name of the ProRes adapter (D24: exactly this; never "Apple ProRes").
pub const PRORES_DISPLAY_NAME: &str = "ProRes (RDD 36 decoder, not Apple-certified)";
/// Decoder id carried by every receipt of the ProRes decoder.
pub const PRORES_DECODER_ID: &str = "prores-rdd36";
/// Bumped whenever any output bit of the ProRes decoder can change.
pub const PRORES_DECODER_VERSION: &str = "1";

/// User-facing name of the DNx adapter.
pub const DNX_DISPLAY_NAME: &str = "DNx / VC-3 (SMPTE ST 2019-1 decoder)";
/// Decoder id carried by every receipt of the VC-3 decoder.
pub const DNX_DECODER_ID: &str = "vc3-st2019-1";
/// Bumped whenever any output bit of the VC-3 decoder can change.
pub const DNX_DECODER_VERSION: &str = "1";

/// True when this build contains the in-process ProRes decoder (cargo feature `prores`).
pub const PRORES_COMPILED_IN: bool = cfg!(feature = "prores");

/// Identity of the in-process ProRes decoder.
pub const PRORES_IDENTITY: DecoderIdentity = DecoderIdentity {
    id: PRORES_DECODER_ID,
    version: PRORES_DECODER_VERSION,
};

/// Identity of the in-process VC-3 decoder.
pub const DNX_IDENTITY: DecoderIdentity = DecoderIdentity {
    id: DNX_DECODER_ID,
    version: DNX_DECODER_VERSION,
};

/// Descriptor of the ProRes adapter. `can_decode` is true only in a build with feature `prores`.
pub fn prores_descriptor() -> CodecDescriptor {
    CodecDescriptor {
        id: "prores",
        name: PRORES_DISPLAY_NAME,
        kind: MediaKind::Video,
        container: "isobmff-mov-mp4",
        bit_depths: &[10],
        chroma: Some("422"),
        transfer: None,
        decoder_id: PRORES_DECODER_ID,
        decoder_version: PRORES_DECODER_VERSION,
        licence: STUDIO_OWNED_LICENCE,
        certification: Certification::NotCertified,
        can_decode: PRORES_COMPILED_IN,
        can_encode: false,
    }
}

/// Descriptor of the DNx / VC-3 adapter (compiled into every build).
pub fn dnx_descriptor() -> CodecDescriptor {
    CodecDescriptor {
        id: "dnx",
        name: DNX_DISPLAY_NAME,
        kind: MediaKind::Video,
        container: "isobmff-mov-mp4",
        bit_depths: &[8, 10],
        chroma: Some("422"),
        transfer: None,
        decoder_id: DNX_DECODER_ID,
        decoder_version: DNX_DECODER_VERSION,
        licence: STUDIO_OWNED_LICENCE,
        certification: Certification::NotCertified,
        can_decode: true,
        can_encode: false,
    }
}

/// The Studio-owned video codec adapters of this build, in a stable order (STU-VID-051 item 2).
pub fn video_codec_descriptors() -> Vec<CodecDescriptor> {
    vec![prores_descriptor(), dnx_descriptor()]
}

/// What an ISO BMFF / QuickTime visual sample entry code means for the video route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SampleEntryRoute {
    /// Decodable through the route for this codec (or typed unavailable when no decoder is chosen).
    Codec(VideoCodec),
    /// Known code that this crate refuses with a stable reason.
    Refused(&'static str),
}

/// Sample entry code -> route. ProRes codes `apco apcs apcn apch` (Proxy, LT, Standard, HQ; the 4:2:2
/// profiles), `ap4h ap4x` (4444, 4444 XQ) and ProRes RAW `aprn aprh` (no public bitstream spec: refused).
/// VC-3 codes `AVdn` (DNxHD) and `AVdh` (DNxHR) as written by ffmpeg's MOV muxer (observed on generated
/// fixtures, see tests/fixtures/dnx/provenance.json).
pub(crate) fn route_for_sample_entry(code: [u8; 4]) -> Option<SampleEntryRoute> {
    match &code {
        b"apco" | b"apcs" | b"apcn" | b"apch" => Some(SampleEntryRoute::Codec(VideoCodec::ProRes)),
        b"ap4h" | b"ap4x" => Some(SampleEntryRoute::Codec(VideoCodec::ProRes)),
        b"aprn" | b"aprh" => Some(SampleEntryRoute::Refused("prores-raw")),
        b"AVdn" | b"AVdh" => Some(SampleEntryRoute::Codec(VideoCodec::Dnx)),
        _ => None,
    }
}
