//! Decode/encode media/proxy-provider adapters; native codec bindings opt-in, never reachable from
//! raster/type default closure.
//!
//! Slice 1 ships license-free, pure-Rust providers behind one instance-owned `Registry`:
//! - `isobmff`: own MP4/MOV index reader (stts/ctts/stss/stsz/stsc/stco/co64, edit lists) with exact PTS,
//!   frame-accurate seek (requested vs actual) and bounded packet delivery. No codec decode.
//! - `wav-pcm`: sample-range decode and exact-header encode.
//! - `y4m`: raw 8-bit video, one frame resident at a time.
//!
//! Compressed codecs are determinate `MEDIA_CODEC_UNAVAILABLE` until a decoder is routed to them. AV1 is
//! routed by policy (`video`): untrusted sources only to a registered EXTERNAL decoder (an isolated worker,
//! see `hsk-studio-reel-native`; available in every build), the in-process rav1d decoder (cargo feature
//! `av1`) only for sources the caller marks `SourceTrust::Trusted`. A proxy is its own source: it is
//! `Untrusted` unless its `ProxySource` is marked otherwise, whatever the original's trust is.
//!
//! The synthetic container builders the tests use (WAV, Y4M, MP4/MOV, Matroska bytes, including hostile
//! variants) are test support in `tests/common`; no module of this crate exports them (PLR-029):
//!
//! ```compile_fail
//! use hsk_studio_reel::fixtures::wav_pcm16;
//! ```
//!
//! A doctest that resolves a real item proves the failure above comes from the missing module, not from
//! a misspelt crate name:
//!
//! ```
//! use hsk_studio_reel::Registry;
//! let _ = Registry::standard();
//! ```
// Default closure: no unsafe. With the opt-in `av1` feature only `av1/ffi.rs` may use it (rav1d C API).
#![cfg_attr(not(feature = "av1"), forbid(unsafe_code))]
#![cfg_attr(feature = "av1", deny(unsafe_code))]

#[cfg(feature = "av1")]
pub mod av1;
mod av1_config;
mod cache;
mod error;
mod identity;
mod isobmff;
mod matroska;
mod mkv_content;
mod mkv_pcm;
mod model;
mod mxf;
mod registry;
mod report;
mod time;
mod video;
mod wav;
mod y4m;

pub use av1_config::{Av1Config, av1_descriptor, parse_av1c};
pub use cache::{CacheStats, IndexCache};
pub use error::{DecoderFault, ReelError};
pub use identity::{
    IdentityParams, RelinkConfidence, RelinkMatch, RelinkOutcome, RelinkQuery, SourceIdentity,
    full_digest, hex, relink, sampled_identity,
};
pub use isobmff::{
    AudioFormat, ColorTag, EditSegment, ISOBMFF_PROVIDER_ID, IsobmffProvider, LpcmDescription,
    MediaIndex, PcmFormat, PcmStream, PcmWindow, PresentedSample, SampleEntry, SeekDisposition,
    SeekPoint, SeekResult, SegmentIndex, SegmentIndexEntry, TimecodeDescription, TimecodeStart,
    TrackIndex, TrackKind, build_index, fourcc_string, pcm_format, read_sample,
    read_timecode_start,
};
pub use matroska::{
    ClusterRef, CuePoint, CuePosition, CuedSeek, MATROSKA_PROVIDER_ID, MatroskaProvider,
    MkvCompression, MkvContentEncoding, MkvEncodingKind, MkvIndex, MkvLayout, MkvTrack,
    MkvTrackInfo, build_matroska_index, cued_seek, read_layout,
};
pub use model::*;
pub use mxf::{
    MXF_PROVIDER_ID, MxfDeltaEntry, MxfDescriptor, MxfEditUnit, MxfElement, MxfIndex,
    MxfIndexEntry, MxfIndexSegment, MxfIndexTable, MxfKlvInfo, MxfPackageKind, MxfPartition,
    MxfPartitionKind, MxfProvider, MxfTrack, MxfTrackKind, MxfWrapping, build_mxf_index,
};
pub use registry::{ProxySource, Registry, detect_type};
pub use report::{outcome_for, report};
pub use video::{
    DecodedPicture, DecoderIdentity, ExternalVideoDecoder, PictureSink, SourceTrust, VideoCodec,
    VideoDecodeError, VideoDecodeSession, VideoRoute, VideoStreamSpec,
};
pub use wav::{WAV_PROVIDER_ID, WavProvider};
pub use y4m::{Y4M_PROVIDER_ID, Y4mProvider};

pub const DESCRIPTOR: &str = r#"{"crate":"hsk-studio-reel","module":"reel","wave":1,"providers":["wav-pcm","y4m","isobmff","matroska"],"registry":"instance-owned, enumerable via Registry::descriptors(); no global factory","default_closure":"pure Rust, accord+observe only, forbid(unsafe_code)","limits":{"max_probe_bytes":67108864,"max_chunk_bytes":8388608,"max_samples":4000000,"max_tracks":64,"max_edit_entries":4096},"errors":["MEDIA_CODEC_UNAVAILABLE","MEDIA_IMPORT_UNSUPPORTED","MEDIA_CORRUPT","MEDIA_TRUNCATED","MEDIA_LIMIT_EXCEEDED","MEDIA_CANCELED","MEDIA_STALE_GRANT","MEDIA_UNSUPPORTED","MEDIA_OUT_OF_RANGE","MEDIA_STRICT_ORIGINAL_REQUIRED","MEDIA_IO","MEDIA_DECODER_FAULT"],"time":"Pulse ticks (accord TICKS_PER_SECOND); non-divisible timescales round to nearest with an explicit ticks_exact receipt flag","slice4":["matroska ContentEncodings: zlib and header stripping undone per frame and for CodecPrivate; encryption and deprecated algorithms are a typed per-track refusal","matroska A_PCM/INT/LIT, A_PCM/INT/BIG, A_PCM/FLOAT/IEEE decode through the shared PCM path","IndexCache serves MkvIndex too (IndexCache::get_or_build_matroska, MatroskaProvider::decode_cached), one capacity shared with MediaIndex","mxf (opt-in provider id mxf, Registry::register(Box::new(MxfProvider))): OP1a / OP-Atom partitions, header metadata tracks and descriptors, index tables (constant and variable edit unit size), frame-accurate byte offsets and essence elements as packets; essence is opaque (decoded requests are MEDIA_CODEC_UNAVAILABLE)"],"slice3":["matroska/webm EBML index (Segment/Info/Tracks/Cluster/SimpleBlock/BlockGroup/Cues, xiph/ebml/fixed lacing, unknown sizes)","cue-based frame-accurate seek","sampled source identity (SHA-256 windows) + relink with confidence and ambiguity"],"slice2":["pcm-in-mov decode (twos/sowt/in24/in32/fl32/fl64/raw/lpcm v2) to little-endian","tmcd raw timecode fields + start sample","fragmented mp4 index (moof/traf/tfhd/tfdt/trun, sidx)","IndexCache keyed by content id + revision + limits"],"video_routing":{"policy":"SourceTrust::Untrusted (default) -> registered ExternalVideoDecoder if it supports the codec, else MEDIA_CODEC_UNAVAILABLE; SourceTrust::Trusted -> in-process decoder when feature av1 is compiled in, else the external decoder, else MEDIA_CODEC_UNAVAILABLE; trust belongs to the decoded source: the trust given to Registry::decode_segment_routed applies to the original only, a ProxySource carries its own trust (ProxySource::new is Untrusted, with_trust grants it for that proxy alone) and never inherits the original's (test trusted_original_never_lends_its_trust_to_a_proxy)","api":["Registry::with_video_decoder","Registry::decode_segment_routed","ProxySource::new","ProxySource::with_trust","MediaProvider::decode_segment_routed","VideoRoute","ExternalVideoDecoder","VideoDecodeSession"],"feature_needed":false,"decoder_faults":["decoder_crashed","decoder_deadline","decoder_protocol"],"picture_checks":"before a decoded picture is packed: its planes must match its format (checked arithmetic) and its size must not exceed the container-declared size, which is the AV1 sequence maximum (a declared side of 0 is not checked), else decoder_protocol; its packed size must fit max_chunk_bytes, else MEDIA_LIMIT_EXCEEDED (tests decoded_picture_size_is_checked_against_the_container_declared_size, oversize_decoded_picture_is_refused_before_it_is_copied)"},"features":{"av1":"optional, default off: IN-PROCESS AV1 decode (rav1d, BSD-2, no asm) for isobmff av01 and matroska V_AV1, only for sources marked trusted; unsafe confined to av1/ffi.rs; rav1d panics abort the process on some corrupted streams, so untrusted input is routed to an external decoder instead"},"not_implemented":["compressed video/audio decode except AV1 behind feature av1","encode beyond wav/y4m","timecode labels (pulse owns)","mxf essence decode, Random Index Pack lookup (the footer chain and a KLV scan find partitions), sparse index tables, essence container labels beyond reporting them","matroska bzlib/lzo1x content compression and content encoding scope 0x4","GOP cache","native codecs (hsk-studio-reel-native)"]}"#;
