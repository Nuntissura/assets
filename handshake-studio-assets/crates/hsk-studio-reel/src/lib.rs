//! Decode/encode media/proxy-provider adapters; native codec bindings opt-in, never reachable from
//! raster/type default closure.
//!
//! Slice 1 ships license-free, pure-Rust providers behind one instance-owned `Registry`:
//! - `isobmff`: own MP4/MOV index reader (stts/ctts/stss/stsz/stsc/stco/co64, edit lists) with exact PTS,
//!   frame-accurate seek (requested vs actual) and bounded packet delivery. No codec decode.
//! - `wav-pcm`: sample-range decode and exact-header encode.
//! - `y4m`: raw 8-bit video, one frame resident at a time.
//!
//! Compressed codecs are determinate `MEDIA_CODEC_UNAVAILABLE` until a licensed/pure provider is added.
// Default closure: no unsafe. With the opt-in `av1` feature only `av1/ffi.rs` may use it (rav1d C API).
#![cfg_attr(not(feature = "av1"), forbid(unsafe_code))]
#![cfg_attr(feature = "av1", deny(unsafe_code))]

#[cfg(feature = "av1")]
pub mod av1;
mod cache;
mod error;
pub mod fixtures;
mod identity;
mod isobmff;
mod matroska;
mod model;
mod registry;
mod report;
mod time;
mod wav;
mod y4m;

pub use cache::{CacheStats, IndexCache};
pub use error::ReelError;
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
    ClusterRef, CuePoint, CuePosition, CuedSeek, MATROSKA_PROVIDER_ID, MatroskaProvider, MkvIndex,
    MkvLayout, MkvTrack, MkvTrackInfo, build_matroska_index, cued_seek, read_layout,
};
pub use model::*;
pub use registry::{ProxySource, Registry, detect_type};
pub use report::{outcome_for, report};
pub use wav::{WAV_PROVIDER_ID, WavProvider};
pub use y4m::{Y4M_PROVIDER_ID, Y4mProvider};

pub const DESCRIPTOR: &str = r#"{"crate":"hsk-studio-reel","module":"reel","wave":1,"providers":["wav-pcm","y4m","isobmff","matroska"],"registry":"instance-owned, enumerable via Registry::descriptors(); no global factory","default_closure":"pure Rust, accord+observe only, forbid(unsafe_code)","limits":{"max_probe_bytes":67108864,"max_chunk_bytes":8388608,"max_samples":4000000,"max_tracks":64,"max_edit_entries":4096},"errors":["MEDIA_CODEC_UNAVAILABLE","MEDIA_IMPORT_UNSUPPORTED","MEDIA_CORRUPT","MEDIA_TRUNCATED","MEDIA_LIMIT_EXCEEDED","MEDIA_CANCELED","MEDIA_STALE_GRANT","MEDIA_UNSUPPORTED","MEDIA_OUT_OF_RANGE","MEDIA_STRICT_ORIGINAL_REQUIRED","MEDIA_IO"],"time":"Pulse ticks (accord TICKS_PER_SECOND); non-divisible timescales round to nearest with an explicit ticks_exact receipt flag","slice3":["matroska/webm EBML index (Segment/Info/Tracks/Cluster/SimpleBlock/BlockGroup/Cues, xiph/ebml/fixed lacing, unknown sizes)","cue-based frame-accurate seek","sampled source identity (SHA-256 windows) + relink with confidence and ambiguity"],"slice2":["pcm-in-mov decode (twos/sowt/in24/in32/fl32/fl64/raw/lpcm v2) to little-endian","tmcd raw timecode fields + start sample","fragmented mp4 index (moof/traf/tfhd/tfdt/trun, sidx)","IndexCache keyed by content id + revision + limits"],"features":{"av1":"optional, default off: AV1 decode (rav1d, BSD-2, no asm) for isobmff av01 and matroska V_AV1; unsafe confined to av1/ffi.rs; TRUSTED INPUT ONLY: rav1d panics abort the process on some corrupted streams"},"not_implemented":["compressed video/audio decode except AV1 behind feature av1","encode beyond wav/y4m","timecode labels (pulse owns)","mxf","matroska content encodings","GOP cache","native codecs (hsk-studio-reel-native)"]}"#;
