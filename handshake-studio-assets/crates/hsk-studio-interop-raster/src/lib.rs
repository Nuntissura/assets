//! Studio raster interop: the single owner (lead decision R-OWN, 2026-10-14; CX-CAP-001) of PNG / JPEG
//! (later WebP, GIF, BMP) container decode and encode over `&[u8]`.
//!
//! Folio-free: the only Studio edges are `hsk-studio-accord` and `hsk-studio-observe`, so engines (develop)
//! and the harness can consume it without pulling Folio or the media crate. Mapping a decoded raster into a
//! Studio document stays with interop-psd's raster proposal and the harness route ([`RasterImage::planes_be`]
//! yields the `Plane` byte layout). JPEG export uses a clean-room baseline encoder (decision J1).
#![forbid(unsafe_code)]
#![deny(clippy::arithmetic_side_effects)]

mod error;
mod jpeg;
mod jpeg_encode;
mod limits;
mod model;
mod png;

pub use error::{RasterError, Result};
pub use jpeg_encode::{encode_jpeg, JpegEncodeRequest, JpegSubsampling};
pub use limits::{Limits, MAX_ENCODE_BYTES};
pub use model::{Colour, JpegInfo, PngInfo, RasterFormat, RasterImage, Samples};
pub use png::{encode_png, PngCompression, PngEncodeRequest};

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Observation, Observe, Outcome, Receipt, SinkPort};

/// The complete public surface (everything else is private).
pub const PUBLIC_ENTRYPOINTS: &[&str] = &["sniff", "decode", "encode_png", "encode_jpeg", "report"];

/// Container signature: PNG `89 50 4E 47 0D 0A 1A 0A`, JPEG `FF D8 FF`, WebP `RIFF....WEBP`.
pub fn sniff(bytes: &[u8]) -> Option<RasterFormat> {
    if bytes.starts_with(png::SIGNATURE) {
        Some(RasterFormat::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(RasterFormat::Jpeg)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(RasterFormat::Webp)
    } else {
        None
    }
}

/// Decodes one still image (the first/default image of an animated file, with a recorded loss).
pub fn decode(bytes: &[u8], limits: &Limits, cancel: &CancellationToken) -> Result<RasterImage> {
    error::check_cancel(cancel)?;
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(RasterError::Limit("input_over_limit"));
    }
    match sniff(bytes) {
        Some(RasterFormat::Png) => png::decode(bytes, limits, cancel),
        Some(RasterFormat::Jpeg) => jpeg::decode(bytes, limits, cancel),
        Some(RasterFormat::Webp) => Err(RasterError::Unsupported("webp_decoder_unavailable")),
        None => Err(RasterError::UnknownFormat),
    }
}

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
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-INTEROP_RASTER","version":1,"scope":"PNG/JPEG container decode, PNG encode and clean-room baseline JPEG encode over byte slices; no Folio mapping (interop-psd raster_proposal + harness route own it), no colour management, orientation and gamma recorded never applied","public_entrypoints":["sniff(bytes) -> Option<RasterFormat>","decode(bytes, Limits, CancellationToken) -> RasterImage","encode_png(PngEncodeRequest) -> Vec<u8>","encode_jpeg(JpegEncodeRequest) -> Vec<u8>","RasterImage::planes_be() -> per-channel big-endian planes (interop-psd Plane layout)","report(result, correlation_id, revision, CancellationToken, Observe, SinkPort) -> Receipt"],"decode":{"png":"all IHDR colour types and depths (gray 1/2/4/8/16, RGB 8/16, palette 1/2/4/8, gray+alpha 8/16, RGBA 8/16), Adam7 interlace; palette, tRNS and sub-byte gray expanded to 8-bit by the codec, 16-bit kept as 16-bit; iCCP inflated (an unreadable iCCP refuses icc_unreadable), gAMA/sRGB/cHRM recorded as stored; APNG: default image only + loss png_animation_not_imported","jpeg":"zune-jpeg 0.5.15 scalar (features std only) behind an own marker walk: baseline (SOF0), extended sequential Huffman (SOF1), progressive (SOF2); 1 component Gray, 3 components RGB (YCbCr converted by the codec), 4 components CMYK as stored (Adobe APP14: inverted, 0 = full ink, JpegInfo.cmyk_inverted) and YCCK converted to inverted CMYK by this crate (BT.601 full range); APP2 ICC_PROFILE reassembled by sequence number (count and sequence consistency required: icc_chunks_inconsistent, more than 255 chunks icc_chunk_count, total over max_icc_bytes icc_over_limit); APP1 Exif orientation recorded (unreadable Exif adds loss jpeg_exif_unreadable); refusals arithmetic_coding_unsupported (SOF9-11, SOF13-15, DAC), jpeg_lossless_unsupported (SOF3), jpeg_hierarchical_unsupported (SOF5-7), jpeg_precision_unsupported (12/16-bit), jpeg_dnl_unsupported (height 0), jpeg_components_unsupported (2 or more than 4), scans_over_limit; samples differ from libjpeg-turbo by at most 3 (mean below 1) on the test fixtures (test jpeg_decode_matches_pillow_for_gray_420_progressive_cmyk_and_exif)","webp":"recognised by sniff, refused webp_decoder_unavailable"},"encode":{"jpeg":"own baseline encoder from ITU-T T.81 (decision J1; no third-party encoder): SOF0 sequential DCT, Huffman with the Annex K.3 tables, Annex K.1/K.2 quantization scaled by quality 1..=100 (q<50: 5000/q %, else 200-2q %, clamped 1..=255), 8-bit Gray or RGB (T.871 YCbCr), 4:4:4 or 4:2:0 (2x2 rounded mean), edge replication, JFIF APP0, ICC in APP2 chunks of 65519 bytes (more than 255 chunks: icc_too_large_for_jpeg); DQT/DHT bytes equal libjpeg-turbo output at the same quality and decode-back PSNR within 0.5 dB of libjpeg-turbo on the same image (tests jpeg_encoder_tables_match_libjpeg_turbo_output, jpeg_encoder_round_trip_psnr_icc_and_determinism); deterministic: f32 FDCT with literal constants and fixed operation order, output hash pinned; refusals jpeg_colour_unsupported (alpha, CMYK), jpeg_depth_unsupported (16-bit), jpeg_quality_out_of_range, jpeg_dims_unsupported (0 or over 65535), sample_count_mismatch, icc_empty","png":"Gray, GrayA, Rgb, Rgba at 8 or 16 bits, optional iCCP; fixed Paeth filter (Stored: none) so bytes are a pure function of the request (test png_round_trip_keeps_samples_icc_and_is_deterministic)"},"hostile_input":"JPEG: marker walk to EOI before the codec (segment lengths and entropy data against the input: truncated; SOF dims: dims_over_limit; output plus codec coefficient/row buffers: decoded_bytes_over_limit; codec max width/height/scans set from Limits). PNG: own chunk pre-scan before the codec: signature, IHDR fields, chunk lengths against the input (truncated), IEND required, dims against max_dimension (dims_over_limit), decoded aggregate with checked arithmetic (decoded_bytes_over_limit, dims_overflow), ancillary bytes (metadata_over_limit); codec allocation limit always set","refusals":["unknown_format","truncated","corrupt_png","icc_unreadable","icc_over_limit","dims_over_limit","decoded_bytes_over_limit","dims_overflow","metadata_over_limit","input_over_limit","corrupt_jpeg","icc_chunks_inconsistent","icc_chunk_count","scans_over_limit","arithmetic_coding_unsupported","jpeg_lossless_unsupported","jpeg_hierarchical_unsupported","jpeg_precision_unsupported","jpeg_dnl_unsupported","jpeg_components_unsupported","jpeg_process_unsupported","jpeg_decoder_layout_mismatch","webp_decoder_unavailable","canceled"],"limits":{"max_input_bytes":1073741824,"max_dimension":65536,"max_decoded_bytes":1073741824,"max_icc_bytes":16777216,"max_metadata_bytes":16777216,"max_jpeg_scans":256,"max_encode_bytes":1073741824},"cancel":"CancellationToken checked before decode, before and after the pixel pass","unverified":["PngSuite corpus run (tests/real_raster.rs, ignored until the corpus is present)"]}"#;
