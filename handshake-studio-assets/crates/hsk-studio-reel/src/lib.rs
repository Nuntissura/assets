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
#![forbid(unsafe_code)]

mod error;
pub mod fixtures;
mod isobmff;
mod model;
mod registry;
mod report;
mod time;
mod wav;
mod y4m;

pub use error::ReelError;
pub use isobmff::{
    AudioFormat, EditSegment, ISOBMFF_PROVIDER_ID, IsobmffProvider, MediaIndex, PresentedSample,
    SampleEntry, SeekDisposition, SeekPoint, SeekResult, TrackIndex, TrackKind, build_index,
    fourcc_string, read_sample,
};
pub use model::*;
pub use registry::{ProxySource, Registry, detect_type};
pub use report::{outcome_for, report};
pub use time::{TickValue, exact_ticks_per_unit, media_to_ticks};
pub use wav::{WAV_PROVIDER_ID, WavProvider};
pub use y4m::{Y4M_PROVIDER_ID, Y4mProvider};

pub const DESCRIPTOR: &str = r#"{"crate":"hsk-studio-reel","module":"reel","wave":1,"providers":["isobmff","wav-pcm","y4m"],"registry":"instance-owned, enumerable via Registry::descriptors(); no global factory","default_closure":"pure Rust, accord+observe only, forbid(unsafe_code)","limits":{"max_probe_bytes":67108864,"max_chunk_bytes":8388608,"max_samples":4000000,"max_tracks":64,"max_edit_entries":4096},"errors":["MEDIA_CODEC_UNAVAILABLE","MEDIA_IMPORT_UNSUPPORTED","MEDIA_CORRUPT","MEDIA_TRUNCATED","MEDIA_LIMIT_EXCEEDED","MEDIA_CANCELED","MEDIA_STALE_GRANT","MEDIA_UNSUPPORTED","MEDIA_OUT_OF_RANGE","MEDIA_STRICT_ORIGINAL_REQUIRED","MEDIA_IO"],"time":"Pulse ticks (accord TICKS_PER_SECOND); non-divisible timescales round to nearest with an explicit ticks_exact receipt flag","not_implemented":["compressed video/audio decode","encode beyond wav/y4m","fragmented mp4","mxf","matroska","GOP cache","relink","native codecs (hsk-studio-reel-native)"]}"#;
