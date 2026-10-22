//! Studio TIFF interop: the single TIFF container owner (lead decision F1, 2026-10-14; CX-CAP-001).
//!
//! Owns TIFF container parsing, decoding and encoding: IFD chain, strips and tiles, decompression,
//! predictors, sample formats, ICC (tag 34675), ExtraSamples, multi-page, BigTIFF, and raw access to the
//! Photoshop tags 37724 (ImageSourceData) and 34377 (ImageResources).
//!
//! Folio-free: the only Studio edges are `hsk-studio-accord` and `hsk-studio-observe`, so the develop engine
//! can consume this crate without gaining a Folio path. Mapping a decoded TIFF into a Studio document belongs
//! to the convert-to-native flow, and the layer records inside tag 37724 are parsed by interop-psd.
#![forbid(unsafe_code)]
#![deny(clippy::arithmetic_side_effects)]

mod container;
mod decode;
mod encode;
mod error;
mod limits;
mod model;
mod psd_block;
mod scan;

pub use container::{Container, Ifd, IfdEntry};
pub use decode::decode;
pub use encode::{encode, ColorModel, Compression, EncodeRequest, Predictor};
pub use error::{Result, TiffError};
pub use psd_block::{image_source_data, ImageSourceData};
pub use limits::{Limits, MAX_ENCODE_BYTES};
pub use model::{
    Alpha, ByteOrder, PageInfo, Photometric, Rational, RawBlock, Resolution, SampleFormat, Samples, TiffFile,
    TiffPage,
};

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Observation, Observe, Outcome, Receipt, SinkPort};

/// The complete public surface (everything else is private).
pub const PUBLIC_ENTRYPOINTS: &[&str] = &["decode", "encode", "image_source_data", "report", "Container"];

/// Maps a decode/encode result to exactly one Observe terminal outcome (no ids, paths or pixel data).
/// Unsupported features are `Failure(Unsupported)`, cancellation `Canceled`, everything else
/// `Failure(Validation)`. `correlation_id` and `revision` must equal the values `observe` was created with.
pub fn report<T>(
    result: &Result<T>,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> std::result::Result<Receipt, hsk_studio_observe::Error> {
    let outcome = match result {
        Ok(_) => Outcome::Success,
        Err(error) => match error.failure_code() {
            None => Outcome::Canceled,
            Some(code) => Outcome::Failure(code),
        },
    };
    observe.emit(
        Observation { correlation_id, revision, outcome, progress: None, private_project_text: None },
        cancel,
        sink,
    )
}

/// Same descriptor for models, the human manual and Argus/diagnostic adapters.
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-INTEROP_TIFF","version":1,"scope":"TIFF container decode/encode over byte slices; no Folio mapping (convert-to-native owns it), no layer-record parsing (interop-psd owns tag 37724 contents), no colour management, orientation recorded never applied","public_entrypoints":["decode(bytes, Limits, CancellationToken) -> TiffFile","encode(EncodeRequest) -> Vec<u8>","image_source_data(RawBlock) -> ImageSourceData","Container::parse(bytes, Limits) -> header, walk() IFDs (next + SubIFD 330, bounded), IfdEntry, bytes/ascii/uints/f64s, region(offset, len); no pixel decode","report(result, correlation_id, revision, CancellationToken, Observe, SinkPort) -> Receipt"],"decode":{"container":"classic TIFF and BigTIFF, both byte orders, top IFD chain (every page), strips and tiles, planar pages interleaved","compressions":"none(1), LZW(5), JPEG(7), Deflate(8, 32946), PackBits(32773); predictors 1,2,3","samples":"u8,u16,u32,f32,f64 interleaved in file order; Gray, RGB, CMYK, ICCLab(9) as stored; WhiteIsZero inverted by the codec; JPEG YCbCr converted to RGB (BT.601, test corpus_files_decode_or_refuse_as_pinned); palette (PI 3, 8 or 16-bit) expanded to 16-bit RGB exactly (equals Pillow output in the corpus test); CIELab (PI 8, 8/16-bit) delivered as stored with signed a/b in the unsigned container","metadata":"alpha kind from ExtraSamples (one unspecified extra sample is kept), extra samples, ICC (34675), resolution, orientation, raw 37724 and 34377 with file byte order; image_source_data validates the signature and the 8BIM/MIB8/8B64/46B8 key","refusals":"photometric_mask, photometric_itulab, photometric_logluv, photometric_unsupported, compression_unsupported(id), bits_unsupported(n) incl. 12-bit and sub-byte, float_bits_unsupported, sample_format_unsupported, bits_heterogeneous, sample_format_heterogeneous, predictor_unsupported, ycbcr_requires_jpeg, ycbcr_coefficients_unsupported, extra_samples_unsupported (more than one extra sample on a colour page), palette_samples_unsupported, cielab_samples_unsupported, image_source_data_key"},"hostile_input":"pre-scan before the codec: header, IFD offsets, entry count (ifd_entry_count), cycles (ifd_cycle), value ranges and strip/tile ranges (truncated), JPEG frame size, declared dimensions with checked arithmetic (dims_overflow, limit_dimension, limit_page_bytes), running aggregate (aggregate_over_budget), page count (limit_pages); tests hostile_inputs_refuse_without_panic and the corpus mutation sweep found no panic, so no catch_unwind is installed","encode":{"colors":"Gray, GrayA, Rgb, Rgba, Cmyk","depth":"8 (u8), 16 (u16), 32 (u32 or f32); 10-bit refused as bit_depth_10_unsupported","compression":"None, Lzw, Deflate (ZIP), PackBits","predictor":"horizontal for integer samples; float predictor refused as predictor_unsupported","extras":"alpha kind, ICC (UNDEFINED), resolution, orientation, BigTIFF on request, little-endian output","round_trip":"encode then decode keeps samples, ICC bytes and alpha kind exactly (tests compressions_and_predictors_round_trip, alpha_icc_bit_depth_and_bigtiff_round_trip)"},"limits":{"max_input_bytes":1073741824,"max_pages":256,"max_ifd_entries":4096,"max_dimension":262144,"max_page_decoded_bytes":268435456,"max_total_decoded_bytes":1073741824,"max_tag_bytes":67108864,"max_chunks":4194304,"max_encode_bytes":1073741824},"cancel":"CancellationToken checked before and between pages; canceled returns canceled and no partial file","diagnostics":"report() emits one Observe terminal outcome; no paths or pixel data are serialized","unverified":["PhotometricInterpretation Photoshop writes for Lab TIFF (D5 oracle session decides)","Photoshop-written layered TIFF (37724) has no corpus file yet","BigTIFF above 4 GiB is not exercised"]}"#;
