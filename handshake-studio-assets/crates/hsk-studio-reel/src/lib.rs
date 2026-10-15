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

pub mod audio;
#[cfg(feature = "av1")]
pub mod av1;
mod av1_config;
mod bitreader;
mod cache;
mod codec_adapters;
#[cfg(test)]
mod delivery_tests;
mod error;
mod h264_config;
mod idct8;
mod identity;
mod isobmff;
mod matroska;
mod mkv_content;
mod mkv_pcm;
mod model;
pub mod mux;
mod mxf;
#[cfg(feature = "prores")]
mod prores;
mod registry;
mod report;
mod time;
mod vc3;
mod video;
mod wav;
mod y4m;

pub use audio::{
    AudioFileKind, AudioFileProvider, DecodedAudio, FLAC_DECODER_ID, FLAC_PROVIDER_ID,
    OGG_PROVIDER_ID, OPUS_DECODER_ID, VORBIS_DECODER_ID, audio_codec_descriptors,
    decode_audio_file, encode_flac,
};
pub use av1_config::{Av1Config, av1_descriptor, parse_av1c};
pub use cache::{CacheStats, IndexCache};
pub use codec_adapters::{
    DNX_DECODER_ID, DNX_DECODER_VERSION, DNX_DISPLAY_NAME, DNX_IDENTITY, PRORES_COMPILED_IN,
    PRORES_DECODER_ID, PRORES_DECODER_VERSION, PRORES_DISPLAY_NAME, PRORES_IDENTITY,
    dnx_descriptor, prores_descriptor, video_codec_descriptors,
};
pub use error::{DecoderFault, ReelError};
pub use h264_config::{
    H264_DECODER_ID, H264_DISPLAY_NAME, H264_LICENCE, H264Config, START_CODE as H264_START_CODE,
    h264_descriptor, nal_unit_type as h264_nal_unit_type, parse_avcc, sample_to_annex_b,
};
pub use identity::{
    IdentityParams, RelinkConfidence, RelinkMatch, RelinkOutcome, RelinkQuery, SourceIdentity,
    full_digest, hex, relink, sampled_identity,
};
pub use isobmff::{
    AudioFormat, CleanAperture, ColorTag, EditSegment, FieldInfo, ISOBMFF_PROVIDER_ID,
    IsobmffProvider, LpcmDescription, MediaIndex, PcmFormat, PcmStream, PcmWindow, PresentedSample,
    SampleEntry, SeekDisposition, SeekPoint, SeekResult, SegmentIndex, SegmentIndexEntry,
    TimecodeDescription, TimecodeStart, TrackIndex, TrackKind, build_index, fourcc_string,
    pcm_format, read_sample, read_timecode_start,
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
#[cfg(feature = "prores")]
pub use prores::{
    ProResFrameHeader, decode_prores_frame, parse_frame_header as parse_prores_frame_header,
};
pub use mux::{
    Container, MUXER_ID, MUXER_VERSION, MuxLimits, MuxReceipt, MuxSpec, Muxer, NclxColor, PcmSpec,
    VideoEntry, VideoTrackSpec, video_timescale,
};
pub use registry::{ProxySource, Registry, detect_type};
pub use report::{outcome_for, report};
pub use vc3::decode_vc3_frame;
pub use video::{
    DecodedPicture, DecoderIdentity, ExternalVideoDecoder, PictureSink, SourceTrust, VideoCodec,
    VideoDecodeError, VideoDecodeSession, VideoRoute, VideoStreamSpec,
};
pub use wav::{WAV_PROVIDER_ID, WavProvider};
pub use y4m::{Y4M_PROVIDER_ID, Y4mProvider};

pub const DESCRIPTOR: &str = r#"{"crate":"hsk-studio-reel","module":"reel","wave":1,"providers":["wav-pcm","y4m","isobmff","matroska"],"registry":"instance-owned, enumerable via Registry::descriptors(); no global factory","default_closure":"pure Rust, accord+observe only, forbid(unsafe_code)","limits":{"max_probe_bytes":67108864,"max_chunk_bytes":8388608,"max_samples":4000000,"max_tracks":64,"max_edit_entries":4096},"errors":["MEDIA_CODEC_UNAVAILABLE","MEDIA_IMPORT_UNSUPPORTED","MEDIA_CORRUPT","MEDIA_TRUNCATED","MEDIA_LIMIT_EXCEEDED","MEDIA_CANCELED","MEDIA_STALE_GRANT","MEDIA_UNSUPPORTED","MEDIA_OUT_OF_RANGE","MEDIA_STRICT_ORIGINAL_REQUIRED","MEDIA_IO","MEDIA_DECODER_FAULT","MEDIA_NOT_EXACT","MEDIA_INCOMPLETE"],"time":"Pulse ticks (accord TICKS_PER_SECOND); non-divisible timescales round to nearest with an explicit ticks_exact receipt flag","slice4":["matroska ContentEncodings: zlib and header stripping undone per frame and for CodecPrivate; encryption and deprecated algorithms are a typed per-track refusal","matroska A_PCM/INT/LIT, A_PCM/INT/BIG, A_PCM/FLOAT/IEEE decode through the shared PCM path","IndexCache serves MkvIndex too (IndexCache::get_or_build_matroska, MatroskaProvider::decode_cached), one capacity shared with MediaIndex","mxf (opt-in provider id mxf, Registry::register(Box::new(MxfProvider))): OP1a / OP-Atom partitions, header metadata tracks and descriptors, index tables (constant and variable edit unit size), frame-accurate byte offsets and essence elements as packets; essence is opaque (decoded requests are MEDIA_CODEC_UNAVAILABLE)"],"slice3":["matroska/webm EBML index (Segment/Info/Tracks/Cluster/SimpleBlock/BlockGroup/Cues, xiph/ebml/fixed lacing, unknown sizes)","cue-based frame-accurate seek","sampled source identity (SHA-256 windows) + relink with confidence and ambiguity"],"slice2":["pcm-in-mov decode (twos/sowt/in24/in32/fl32/fl64/raw/lpcm v2) to little-endian","tmcd raw timecode fields + start sample","fragmented mp4 index (moof/traf/tfhd/tfdt/trun, sidx)","IndexCache keyed by content id + revision + limits"],"export":{"rule":"Purpose::Export is exact or it fails: a start that is not a whole unit of the source is MEDIA_NOT_EXACT (requested, nearest) before the first payload read, in the isobmff, matroska, wav-pcm, y4m and mxf providers, and for compressed video before a decoder session is opened (nearest is a start the export can be retried at: for a start inside an edit-trimmed first sample it is the next whole presented sample); a compressed-video decode that delivers fewer pictures than requested is MEDIA_INCOMPLETE (requested, delivered); a range that runs past the end of the presentation is MEDIA_OUT_OF_RANGE and a range whose edit list presents one media sample more than once (edits with overlapping media ranges) is MEDIA_INCOMPLETE (requested, delivered = different media samples), both before the first payload read (the range check is test export_range_past_the_end_is_refused_before_the_first_read_for_every_provider; a y4m preview of a range past the last complete frame, whose header declares no frame count, keeps streaming the frames that exist and ends MEDIA_TRUNCATED with the delivered count) and, for compressed video, before a decoder session opens; the packet, wav, y4m, PCM and mxf receipts take complete, actual.units and the end of the actual span from what their delivery loop delivered, not from the request (model::delivery_complete, unit test delivery_complete_is_computed_from_delivered_units; tests wav_receipt_states_what_the_loop_delivered, y4m_receipt_states_what_the_loop_delivered, pcm_receipt_states_what_the_loop_delivered and mxf_receipt_states_what_the_loop_delivered stop the loop short and read the receipt) and a preview of a repeating range is Partial with complete false, its actual span ends where the delivered chunks end and its requested span at the end of the presented range (test repeated_media_sample_is_a_shortfall_not_a_silent_success); a compressed-video preview whose decoder dropped a picture is Partial and its actual span runs from the start of the earliest delivered picture to the end of the delivered picture that ends latest, whatever order the decoder released them in and whether it releases them as it goes or holds them until it finishes (an empty span at the packet start when none was delivered), while a complete decode keeps the packet span (tests partial_video_receipt_spans_the_delivered_pictures, partial_video_receipt_over_a_packet_short_range_follows_the_delivered_pictures, partial_video_receipt_follows_pictures_released_out_of_order, fully_delivered_video_receipt_end_is_the_packet_end; for a decoder that holds its pictures until it finishes partial_video_receipt_spans_the_pictures_a_decoder_holds_until_finish, partial_video_receipt_follows_out_of_order_pictures_held_until_finish, fully_delivered_video_receipt_with_pictures_held_until_finish_is_the_packet_receipt); an mxf index whose temporal offsets do not map the requested display positions one-to-one onto coded positions is MEDIA_CORRUPT mxf-temporal-offset before any essence is read, for Preview as for Export; Purpose::Preview keeps Approximate and Partial receipts; rounding of a media time to ticks stays the ticks_exact receipt flag","tests":["export_is_exact_or_error_for_every_provider","export_refuses_partial_video_decode","nearest_of_a_trimmed_first_sample_is_a_start_export_accepts","export_not_exact_is_refused_before_a_decoder_session_opens","mxf_temporal_offsets_that_are_not_a_permutation_are_refused","repeated_media_sample_is_a_shortfall_not_a_silent_success","export_range_is_checked_before_a_decoder_session_opens","delivery_complete_is_computed_from_delivered_units","export_range_past_the_end_is_refused_before_the_first_read_for_every_provider","wav_receipt_states_what_the_loop_delivered","y4m_receipt_states_what_the_loop_delivered","pcm_receipt_states_what_the_loop_delivered","mxf_receipt_states_what_the_loop_delivered","partial_video_receipt_spans_the_delivered_pictures","partial_video_receipt_over_a_packet_short_range_follows_the_delivered_pictures","partial_video_receipt_follows_pictures_released_out_of_order","fully_delivered_video_receipt_end_is_the_packet_end","partial_video_receipt_spans_the_pictures_a_decoder_holds_until_finish","partial_video_receipt_follows_out_of_order_pictures_held_until_finish","fully_delivered_video_receipt_with_pictures_held_until_finish_is_the_packet_receipt"]},"isobmff_index":{"zero_duration_edit":"an elst entry with segment_duration 0: an empty edit is no segment; a non-empty one is open-ended only as the last edit of a fragmented file, anywhere else the track is refused: the file is still indexed, the other tracks deliver as usual, and that track reports TrackIndex::unsupported() == Some(\"elst-zero-duration\") with seek, probe, every delivery and read_timecode_start of it (as the named track and as a referenced tmcd track) MEDIA_UNSUPPORTED elst-zero-duration, and the window of its compact PCM stream the same refusal (tests fragmented_zero_duration_edit, zero_duration_edit_costs_only_its_track, refused_track_is_unsupported_for_every_use)","sample_entry_metadata":"fiel (FieldInfo) and clap (CleanAperture) are exposed raw, without interpretation, and do not fail a track (test fiel_and_clap_are_raw_and_advisory)","oracle":"an ffmpeg-written MOV with B-frames, negative ctts, an edit list and a drop-frame tmcd track pins presentation times, sync samples and the timecode start (test ffmpeg_made_mov_matches_independent_oracle)"},"video_routing":{"policy":"SourceTrust::Untrusted (default) -> registered ExternalVideoDecoder if it supports the codec, else MEDIA_CODEC_UNAVAILABLE; SourceTrust::Trusted -> in-process decoder when feature av1 is compiled in, else the external decoder, else MEDIA_CODEC_UNAVAILABLE; trust belongs to the decoded source: the trust given to Registry::decode_segment_routed applies to the original only, a ProxySource carries its own trust (ProxySource::new is Untrusted, with_trust grants it for that proxy alone) and never inherits the original's (test trusted_original_never_lends_its_trust_to_a_proxy)","api":["Registry::with_video_decoder","Registry::decode_segment_routed","ProxySource::new","ProxySource::with_trust","MediaProvider::decode_segment_routed","VideoRoute","ExternalVideoDecoder","VideoDecodeSession"],"feature_needed":false,"decoder_faults":["decoder_crashed","decoder_deadline","decoder_protocol"],"picture_checks":"before a decoded picture is packed: its planes must match its format (checked arithmetic) and its size must not exceed the container-declared size, which is the AV1 sequence maximum (a declared side of 0 is not checked), else decoder_protocol; its packed size must fit max_chunk_bytes, else MEDIA_LIMIT_EXCEEDED (tests decoded_picture_size_is_checked_against_the_container_declared_size, oversize_decoded_picture_is_refused_before_it_is_copied)","picture_matching":"a picture belongs to the packet whose tag it carries; a session that releases in packet (decode) order (VideoDecodeSession::reorders_output false, the default) passes over every earlier pending packet; a session that declares reorders_output (pictures leave in presentation order, e.g. H.264 with B-frames through the Media Foundation worker) has each picture matched to its own packet and passes over only the pending packets presented before it, so no picture of a reordered stream is lost and the sink receives pictures in presentation order; a picture of a packet already passed over is not delivered and the receipt reports the shortfall (test pictures_released_in_presentation_order_are_matched_by_tag)"},"codec_adapters":{"contract":"STU-VID-051 as enriched by D23 (staged delta STUDIO-ENRICHMENT-20261014): Studio-owned pure-Rust adapters behind the same descriptor, enumeration (Registry::video_codec_descriptors) and MEDIA_CODEC_UNAVAILABLE contract; every descriptor carries name, decoder_id, decoder_version, licence and certification","dnx":"default build (D24): VC-3 decoder vc3-st2019-1 written from SMPTE ST 2019-1:2016 + Amd 1:2023 for MOV sample entries AVdn/AVdh; progressive frame-encoded YCbCr 4:2:2/4:2:0 at 8/10 bit without alpha for the Compression IDs of Annex C; interlaced, CID 1260, alpha, 4:4:4, RGB and 12-bit are typed MEDIA_UNSUPPORTED (tests tests/vc3_decode.rs)","prores":"feature prores only (default off, D24): decoder prores-rdd36 written from SMPTE RDD 36:2022, name ProRes (RDD 36 decoder, not Apple-certified), certification not_certified; apco apcs apcn apch ap4h ap4x; 4:2:2 (10-bit output) and 4:4:4 (12-bit output), progressive and interlaced; encoded alpha is MEDIA_UNSUPPORTED prores-alpha; without the feature ProRes is MEDIA_CODEC_UNAVAILABLE; aprn/aprh (ProRes RAW) are MEDIA_UNSUPPORTED prores-raw in every build (tests tests/prores_decode.rs)","trust":"built-in pure-Rust decoders run for any SourceTrust; the IDCT is the crate reference src/idct8.rs (f64, constant basis, fixed order)"},"audio_codecs":{"lane":"SMB-AUDIO-CODECS","contract":"STU-VID-050/051 as enriched by D23: each audio codec is a declared adapter (audio_codec_descriptors, Registry::audio_codec_descriptors) with decoder id, version, licence and certification; standalone file providers are opt-in (Registry::register_audio_codecs / with_audio_codecs, like mxf), standard() is unchanged","decoded_output":"ChunkKind::AudioSamples chunks of interleaved little-endian f32 at the stream rate; integer PCM scaled by 2^-(bits-1)","flac":"decoder flac-rfc9639 written from RFC 9639 (CONSTANT/VERBATIM/FIXED/LPC subframes, wasted bits, Rice escapes, stereo decorrelation, header CRC-8, frame CRC-16, STREAMINFO MD5), bit-exact against libavcodec on two ffmpeg-written files with the MD5 verified (test flac_decode_is_bit_exact_against_libavcodec_and_streaminfo_md5); encoder (FIXED orders 0-4, partitioned Rice, stereo decorrelation, 4-24 bit, 1-8 channels) round-trips bit-exact through encode_flac and the provider encode_segment port (test flac_encoder_round_trips_bit_exact); hostile headers are refused before allocation (tests flac_hostile_headers_are_refused_before_allocation, flac_aggregate_over_budget)","ogg":"provider ogg-audio: own RFC 3533 reader (page CRC 0x04C11DB7, packets bounded while they grow, last-page granule by a bounded tail scan) selecting the first Vorbis / Opus / FLAC stream; Vorbis via lewton 0.10.2 behind a setup-header codebook budget pre-scan (comment header never parsed), within 0.5/32768 of libavcodec (test vorbis_matches_libavcodec_within_two_lsb); Opus via opus-pure 0.2.2 with reel-side OpusHead/dOps parsing, output gain, pre-skip and granule end trim and its own RFC 6716 Appendix B multistream splitter, within 0.5/32768 of libopus on stereo and 5.1 (test opus_stereo_and_surround_match_libopus; 5.1 delivered in the stream Vorbis order); Ogg FLAC bit-exact (test ogg_flac_is_bit_exact_and_segments_are_positioned); hostile pages and headers (test ogg_hostile_input_is_refused_before_allocation); third-party decoder panics become MEDIA_DECODER_FAULT decoder_crashed","mpeg_audio":"provider mpeg-audio: Studio-owned safe-Rust port of minimp3 (CC0-1.0; MPEG-1/2/2.5 Layers I, II, III, free format; bit reader and reservoir bounded, table lookups checked, output finite and within MP3_OUTPUT_BOUND 8.0); ID3v2 skipped against the probe budget, Xing/Info frame not decoded, LAME gapless trim (encoder delay + 529, padding), header walk for the total without Xing; within 0.5-1/32768 of libavcodec on Layer III joint-stereo CBR, MPEG-2 LSF mono VBR with short blocks and Layer II (tests mp3_cbr_stereo_matches_libavcodec, mp3_lsf_mono_vbr_with_short_blocks_matches_libavcodec, mp2_layer2_matches_and_segments_are_positioned); truncation and byte-flip sweeps typed, never a panic (test mpeg_audio_hostile_input_is_typed)","limits":{"max_sample_rate":768000,"max_channels":8}},"features":{"prores":"optional, default off: in-process ProRes decoder (see codec_adapters.prores)","av1":"optional, default off: IN-PROCESS AV1 decode (rav1d, BSD-2, no asm) for isobmff av01 and matroska V_AV1, only for sources marked trusted; unsafe confined to av1/ffi.rs; rav1d panics abort the process on some corrupted streams, so untrusted input is routed to an external decoder instead"},"not_implemented":["compressed video/audio decode except AV1 behind feature av1, VC-3 (default) and ProRes behind feature prores","encode beyond wav/y4m","timecode labels (pulse owns)","mxf essence decode, Random Index Pack lookup (the footer chain and a KLV scan find partitions), sparse index tables, essence container labels beyond reporting them","matroska bzlib/lzo1x content compression and content encoding scope 0x4","GOP cache","native codecs (hsk-studio-reel-native)"]}"#;
