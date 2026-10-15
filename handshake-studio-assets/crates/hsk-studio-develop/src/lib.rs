//! RAW decode/develop recipes; originals immutable through CKC/ArtifactService provider identity,
//! target-independent scalar oracle and SIMD dispatch. Imported catalog semantics adapt into
//! existing CKC owner, not a private Studio catalog.
//!
//! Slice 1: native process 1.0 / engine 1.0 (`native_cfa_bilinear_hermite_v1`) over caller-granted
//! decoded camera CFA bytes (STU-CON-042..045). Scalar oracle only; no SIMD, no unsafe.
//!
//! Throughput: normalisation row bands and output tiles run on std scoped threads
//! ([`Threads`], default `min(available_parallelism, 16)`; the `*_threads` entry points take an
//! explicit count). Tiles, receipts and refusals are byte-identical for every count and the lowest
//! failing tile decides the error (`tests/develop_parallel.rs`: `output_is_identical_for_every_thread_count`,
//! `lowest_failing_tile_decides_the_error_for_every_thread_count`).
#![forbid(unsafe_code)]

pub mod adjust;
mod arw;
pub mod colour;
pub mod color;
pub mod convert;
mod cr2;
pub mod curves;
pub mod detail;
pub mod local;
pub mod output;
pub mod dng;
pub mod pipeline;
pub mod recipe;
pub mod ljpeg;
mod nef;
mod nefc;
mod par;
mod tiffep;

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{FailureCode, Observation, Observe, Outcome, SinkPort};

pub use adjust::{
    ADJUST_MIN_VERSION, ADJUST_VERSION, AbsoluteWhiteBalance, TINT_UV_PER_COUNT, white_xy, apply_pixel, clarity, planck_xy, Adjustments, BAND_CENTRES, BAND_NAMES, BasicTone, ColourMixer, HslBand, ParametricCurve,
    Presence, WhiteBalanceShift,
};
pub use color::{CameraSolution, DngColor};
pub use convert::{
    BandLimits, ConversionJob, ConversionStats, IccProfile, PrismContext, PrismConverter,
    ProfileConverter, WIDE_CHUNK_PIXELS,
};
pub use local::{
    Brush, ColourRange, Correction, LinearGradient, LuminanceRange, MAX_COLOUR_SAMPLES, LocalStack, Mask, MaskBlend, MaskSource, RadialGradient, parse_dab,
};
pub use colour::{ColourGrading, GrayscaleMix, MAX_POINT_COLOURS, PointColour, Wheel};
pub use detail::{Detail, NoiseReduction, Sharpen};
pub use curves::{Curve, PreparedCurve};
pub use dng::{recipe_for_white_point, ActiveArea, BaselineExposure, DngRaw, PlaneWindow, RangePolicy, WindowReport};
pub use output::{
    ClipReport, MAX_ICC_BYTES, OutputDepth, OutputImage, check_icc_header, develop_to_output, render_output,
};
pub use par::{DEFAULT_THREAD_CAP, MAX_THREADS, Threads};
pub use pipeline::{
    Bayer, Cfa, CfaPattern, CfaPlane, DevelopReceipt, DevelopRequest, DevelopedTile,
    DevelopedImage, ExecutionHint, Limits, PlaneInit, SinkError, demosaic_normalized,
    demosaic_normalized_threads, develop, develop_to_image, develop_to_image_threads, develop_with,
    develop_with_threads,
};
pub use recipe::{
    DEMOSAIC_ALGORITHM, DevelopRecipe, ENGINE_VERSION, Enables, MATH_TOKEN, NormalizedCrop,
    PROCESS_VERSION, ProfileBinding, ResolvedCurves, WbMode,
};

/// Every refusal is typed and carries a stable snake_case code; nothing is clipped or defaulted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DevelopError {
    UnsupportedProcessVersion,
    UnsupportedMathToken,
    UnsupportedDemosaic,
    UnsupportedWhiteBalance,
    UnsupportedCrop,
    UnsupportedProfileTransform,
    UnsupportedExecution,
    UnsupportedPattern,
    UnsupportedCurve,
    UnsupportedContainer,
    UnsupportedCompression,
    UnsupportedActiveArea,
    UnsupportedIlluminant,
    UnsupportedContribution(&'static str),
    /// Prism admitted-route refusal; carries Prism's stable error code.
    Prism(&'static str),
    MissingColourData,
    ColorNoConvergence,
    NonFinite,
    NotInvertibleMatrix,
    InvalidProfile,
    InvalidCurve,
    /// An adjustment parameter outside its documented range or an unusable split/matrix.
    InvalidAdjustment,
    /// A local-adjustment mask or correction with unusable geometry, counts or size.
    InvalidMask,
    CurveDomain,
    InvalidDimensions,
    InvalidStride,
    InvalidLength,
    InvalidLevels,
    InvalidPhase,
    InvalidGrid,
    InvalidLimits,
    InvalidContainer,
    SampleOutOfRange,
    NormalizationUnderflow,
    StageUnderflow,
    StageOverflow,
    MissingColourSupport,
    EmptyCrop,
    TooManyTiles,
    OutputTooLarge,
    Overflow,
    StaleSource,
    Canceled,
    SinkRejected,
    /// A recognised container family without a decoder in this crate; carries the family
    /// (`orf`, `rw2`, `raf`, `cr3`, `jpeg`, `nef_compressed`) or `unknown` for unrecognised bytes.
    DecoderUnavailable(&'static str),
}

impl DevelopError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedProcessVersion => "unsupported_process_version",
            Self::UnsupportedMathToken => "unsupported_math_token",
            Self::UnsupportedDemosaic => "unsupported_demosaic",
            Self::UnsupportedWhiteBalance => "unsupported_white_balance",
            Self::UnsupportedCrop => "unsupported_crop",
            Self::UnsupportedProfileTransform => "unsupported_profile_transform",
            Self::UnsupportedExecution => "unsupported_execution",
            Self::UnsupportedPattern => "unsupported_pattern",
            Self::UnsupportedCurve => "unsupported_curve",
            Self::UnsupportedContainer => "unsupported_container",
            Self::UnsupportedCompression => "unsupported_compression",
            Self::UnsupportedActiveArea => "unsupported_active_area",
            Self::UnsupportedIlluminant => "unsupported_illuminant",
            Self::MissingColourData => "missing_colour_data",
            Self::ColorNoConvergence => "color_no_convergence",
            Self::UnsupportedContribution(_) => "unsupported_contribution",
            Self::Prism(_) => "prism_refused",
            Self::NonFinite => "non_finite",
            Self::NotInvertibleMatrix => "not_invertible_matrix",
            Self::InvalidProfile => "invalid_profile",
            Self::InvalidCurve => "invalid_curve",
            Self::InvalidAdjustment => "invalid_adjustment",
            Self::InvalidMask => "invalid_mask",
            Self::CurveDomain => "curve_domain",
            Self::InvalidDimensions => "invalid_dimensions",
            Self::InvalidStride => "invalid_stride",
            Self::InvalidLength => "invalid_length",
            Self::InvalidLevels => "invalid_levels",
            Self::InvalidPhase => "invalid_phase",
            Self::InvalidGrid => "invalid_grid",
            Self::InvalidLimits => "invalid_limits",
            Self::InvalidContainer => "invalid_container",
            Self::SampleOutOfRange => "sample_out_of_range",
            Self::NormalizationUnderflow => "normalization_underflow",
            Self::StageUnderflow => "stage_underflow",
            Self::StageOverflow => "stage_overflow",
            Self::MissingColourSupport => "missing_colour_support",
            Self::EmptyCrop => "empty_crop",
            Self::TooManyTiles => "too_many_tiles",
            Self::OutputTooLarge => "output_too_large",
            Self::Overflow => "overflow",
            Self::StaleSource => "stale_source",
            Self::Canceled => "canceled",
            Self::SinkRejected => "sink_rejected",
            Self::DecoderUnavailable(_) => "decoder_unavailable",
        }
    }

    /// Observe outcome class: unsupported -> Unsupported, missing decoder -> Unavailable,
    /// cancellation is its own outcome, everything else is validation.
    pub fn outcome(self) -> Outcome {
        match self {
            Self::Canceled => Outcome::Canceled,
            Self::Prism(code) if code.starts_with("unsupported") => {
                Outcome::Failure(FailureCode::Unsupported)
            }
            Self::Prism("admission_denied" | "retirement_unavailable") => {
                Outcome::Failure(FailureCode::Unavailable)
            }
            Self::UnsupportedProcessVersion
            | Self::UnsupportedMathToken
            | Self::UnsupportedDemosaic
            | Self::UnsupportedWhiteBalance
            | Self::UnsupportedCrop
            | Self::UnsupportedProfileTransform
            | Self::UnsupportedExecution
            | Self::UnsupportedPattern
            | Self::UnsupportedCurve
            | Self::UnsupportedContainer
            | Self::UnsupportedCompression
            | Self::UnsupportedActiveArea
            | Self::UnsupportedIlluminant
            | Self::UnsupportedContribution(_) => Outcome::Failure(FailureCode::Unsupported),
            Self::DecoderUnavailable(_) | Self::SinkRejected => {
                Outcome::Failure(FailureCode::Unavailable)
            }
            _ => Outcome::Failure(FailureCode::Validation),
        }
    }
}

impl std::fmt::Display for DevelopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedContribution(name) => write!(f, "unsupported_contribution:{name}"),
            Self::Prism(code) => write!(f, "prism_refused:{code}"),
            Self::DecoderUnavailable(format) => write!(f, "decoder_unavailable:{format}"),
            other => f.write_str(other.code()),
        }
    }
}
impl std::error::Error for DevelopError {}

impl From<hsk_studio_accord::ValidationError> for DevelopError {
    fn from(error: hsk_studio_accord::ValidationError) -> Self {
        match error {
            hsk_studio_accord::ValidationError::Canceled => Self::Canceled,
            _ => Self::InvalidProfile,
        }
    }
}

/// Container entry point. Classic TIFF with the Canon `CR\x02` marker decodes as CR2 (stored CFA
/// frame, colour caller-supplied); a classic TIFF whose IFD0 Make is NIKON and that has no
/// DNGVersion decodes as NEF (uncompressed only; Nikon compressions are
/// `decoder_unavailable:nef_compressed`);
/// Make SONY without DNGVersion decodes as ARW (cRAW Compression 32767 or uncompressed); other
/// TIFF-family bytes go to the DNG reader (no DNGVersion = `unsupported_container`). ORF, RW2,
/// RAF, CR3 and JPEG are recognised by their magic and refused as `decoder_unavailable` naming
/// the family; anything else is
/// `decoder_unavailable:unknown` (STU-CON-042: typed refusal).
pub fn decode_container(bytes: &[u8]) -> Result<DngRaw, DevelopError> {
    // TIFF family (classic + BigTIFF magic); BigTIFF is then refused by the reader, not mislabeled.
    if [&b"II*\0"[..], b"MM\0*", b"II+\0", b"MM\0+"]
        .iter()
        .any(|magic| bytes.starts_with(magic))
    {
        if cr2::is_cr2(bytes) {
            cr2::decode(bytes)
        } else if nef::is_nef(bytes) {
            nef::decode(bytes)
        } else if arw::is_arw(bytes) {
            arw::decode(bytes)
        } else {
            dng::decode(bytes)
        }
    } else {
        Err(DevelopError::DecoderUnavailable(recognise(bytes)))
    }
}

/// Name of a recognised non-TIFF raw family (magic bytes only; nothing else is read).
fn recognise(bytes: &[u8]) -> &'static str {
    if [&b"IIRO"[..], b"IIRS", b"MMOR"].iter().any(|m| bytes.starts_with(m)) {
        "orf"
    } else if bytes.starts_with(b"IIU\0") {
        "rw2"
    } else if bytes.starts_with(b"FUJIFILMCCD-RAW") {
        "raf"
    } else if bytes.get(4..12) == Some(&b"ftypcrx "[..]) {
        "cr3"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpeg" // a rendered JPEG (e.g. a .dng download that is really a JFIF preview), not raw
    } else {
        "unknown"
    }
}

/// Deliver the terminal outcome of one develop run through the caller-owned sink.
/// `correlation_id` and `revision` must match the `Observe` instance.
pub fn report(
    result: &Result<DevelopReceipt, DevelopError>,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
    correlation_id: u64,
    revision: u64,
) -> Result<hsk_studio_observe::Receipt, hsk_studio_observe::Error> {
    let outcome = match result {
        Ok(_) => Outcome::Success,
        Err(error) => error.outcome(),
    };
    observe.emit(
        Observation {
            correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        cancel,
        sink,
    )
}

pub const DESCRIPTOR: &str = r#"{"crate":"hsk-studio-develop","module":"develop","slice":1,"scope":"source-only decoded-CFA develop over caller-granted bytes plus an own DNG reader (classic TIFF, uncompressed 8/16-bit or lossless JPEG, strips or tiles, ColorMatrix/ForwardMatrix) and an own Canon CR2 reader (CR 2.0, one lossless-JPEG strip, RawImageSegmentation column slices reassembled; stored CFA frame with masked borders, black 0 and full-scale white as placeholders named in unapplied, RGGB at the stored origin UNVERIFIED, no colour: develop needs a caller-supplied DngColor else missing_colour_data; tests/develop_cr2.rs) and an own Nikon NEF reader (uncompressed TIFF/EP CFA SubIFD, 8/16-bit or big-endian MSB-first packed 9-15-bit strips, TIFF/EP CFAPattern; black 0 and full-scale white as placeholders named in unapplied, no colour; tests/develop_nef.rs) and a Sony ARW reader adapted from rawkit 0.1.0 (MIT OR Apache-2.0, see NOTICE): cRAW Compression 32767 (16-pixel groups, 11-bit max/min, 7-bit deltas, SonyToneCurve expansion) or uncompressed strips, TIFF/EP CFAPattern, no colour; tests/develop_arw.rs); no catalog, no persistence, no host wiring","process_version":"1.0","engine_version":"1.0","math_token":"native_cfa_bilinear_hermite_v1","demosaic_algorithm":"bilinear_reflect_native_v1","cfa":["RGGB","BGGR","GRBG","GBRG","xtrans_6x6"],"white_balance_modes":["as_shot"],"profile_transform":{"identity":"working equals output (inspected when a converter is supplied)","prism_admitted_wide":"develop_with + PrismConverter; both profiles inspected through the admitted Prism route (linear-light RGB profiles only), pixels converted by a Prism AdmittedWideEngine job (prepare_job; one band per tile, a tile above 262144 pixels is several bands, limit 2097152 per band and 65536 bands per job) that the tile workers share and convert in place between stage A and stage C (the plan is resolved once per job; each profile is parsed once per cached plan; receipt profile_stats = plan hits/misses/parses per job, profile_calls = bands); working RGB must be finite in 0..1 else prism refuses it as channel_range with no tile delivered (tests/develop_profile.rs: profile_transform_admitted); relative colorimetric, BPC off; output unclamped, destination values outside 0..1 counted in profile_stats.clip (tests/develop_profile.rs: wide_requests_are_chunk_invariant_and_report_plan_cache_and_clip); exposure and curves run after conversion; every wide-path allocation (engine, then at prepare: work, receipt, plan and transient build leases; band conversion allocates nothing, Prism tests/wide_chunks.rs: chunk_accounting_is_bounded_deterministic_and_allocation_free) is charged to the caller admission ledger first and a denial is prism_refused:admission_denied before any parse (tests/develop_profile.rs: short_admission_budget_is_refused_before_any_work)","chunking":"the STU-CON-044 64-pixel chunks are Prism's, not develop's: every band is converted inside Prism in chunks of at most 64 pixels (hsk_studio_prism::ADMITTED_CHUNK_PIXELS), each chunk starting with a cancellation check, recorded in Prism's band and job receipts (test names checked against Prism 3312f72: Prism tests/wide_chunks.rs chunk_accounting_is_bounded_deterministic_and_allocation_free, admitted_transform_and_job_report_their_chunks, a_canceled_token_stops_a_band_before_its_first_chunk; Prism src/wide_job/tests.rs a_band_canceled_at_a_chunk_edge_leaves_exactly_the_chunks_before_it, an_in_place_band_canceled_mid_band_is_partly_overwritten); a develop band is a whole tile (cut only above 262144 pixels), one band call per tile and not one admitted call per 64 pixels (the literal reading, not adopted); develop neither reads nor reports the chunk records (DevelopReceipt has no chunk field) and no develop test asserts a chunk count or cancels between the chunks of a real Prism band; develop tests run its bands through the chunked code and require bit identity with the serial admitted transform (tests/develop_profile.rs: job_conversion_equals_the_serial_wide_transform_for_every_thread_count) and an already cancelled token to stop a Prism band (tests/develop_profile.rs: job_contract_violations_keep_their_prism_codes); the reading is provisional (the Operator may require the literal reading or enrich the spec)"},"curves":"monotone_hermite_2_to_32_points_linear_output_domain","execution":{"supported":["scalar","auto"],"auto_route":"scalar","unsupported":["sse2"],"threads":{"api":"develop_with_threads / develop_to_image_threads / demosaic_normalized_threads (Threads)","default":"min(available_parallelism, 16)","max":64,"unit":"normalisation row bands and output tiles (std scoped threads, no unsafe)","deterministic":"tiles, receipt and refusals are identical for every thread count; the lowest failing tile decides the error; cancellation between normalisation row bands and between tiles returns canceled with no partial output (tests/develop_parallel.rs: output_is_identical_for_every_thread_count, lowest_failing_tile_decides_the_error_for_every_thread_count, cancellation_returns_canceled_and_no_partial_output, which cancels from inside a stand-in converter band); the token is also passed to every converter band, where Prism checks it per 64-pixel chunk (profile_transform.chunking)","converter":"one converter job per window of tiles is prepared before any tile is computed; each worker converts its own tile bands in place between stage A and stage C and the job is finished once on the calling thread (tests/develop_parallel.rs: converter_bands_run_once_each_on_the_tile_workers)"}},"limits":{"spec_v1":{"side_min_bayer":2,"side_min_xtrans":6,"side_max":256,"stride_max_bytes":1024,"plane_max_bytes":262144,"tile_edge_max":64,"tiles_max":16,"output_max_bytes":16777216}},"output":{"colour":"rgb_f32le linear_light","alpha":"f32le straight, 1.0 inside crop only","coverage":"coverage_u8"},"supported_enables":["enable_tone_curve","enable_colour_adjustments","enable_detail","enable_mask_group_based_corrections"],"controls":{"version":"1.0","always_on":"white balance shift (incremental temperature/tint, Bradford in the working space), contrast, highlights, shadows, whites, blacks (monotone PCHIP in the sRGB-encoded domain)","enable_colour_adjustments":"vibrance, saturation, 8-band HSL mixer","enable_tone_curve":"parametric regions (with the point curves)","enable_detail":"clarity (whole frame; develop_to_image only, tile API refuses clarity_whole_frame)","enable_mask_group_based_corrections":"local corrections: linear/radial gradient and brush masks, normalised local stack (whole frame; develop_to_image only, tile API refuses mask_group_whole_frame)","output":"render_output / develop_to_output: sRGB-TRC 8/16-bit RGB + ICC bytes (header and recipe hash checked), clipped values counted","parity":"Lightroom-matching values UNVERIFIED (D5 oracle)","tests":"tests/develop_adjust.rs, develop_local.rs, develop_output.rs"},"refusals":"typed, stable snake_case codes; nothing clipped, defaulted or silently zeroed","pending":["spec STU-CON-044 64-pixel chunk text: read provisionally as met by Prism's 64-pixel chunking inside every band (profile_transform.chunking); develop itself makes one band call per tile, so the Operator may still require one admitted call per 64 pixels or enrich the spec","SIMD route","non-linear (encoded) profiles (refused by the linear-light admitted inspection, typed unsupported_profile for the encoded fixture in tests/develop_profile.rs: profile_transform_admitted; Prism's wide route can convert them) and BPC or an intent other than relative colorimetric (the converter fixes relative colorimetric with BPC off; Prism's wide route has both)","host grants and publication","DNG compressions other than none and lossless JPEG (deflate, lossy JPEG)","proprietary RAW: NEF Nikon compressions 34713/32769 (decoder_unavailable:nef_compressed: no public documentation of the Huffman trees and no permissive decoder source), RW2 Panasonic compression (decoder_unavailable:rw2, same reason), NEF MakerNote black/white levels and curve (named in DngRaw.unapplied: nef_black_level_unread, nef_white_level_unread, nef_makernote_curve_unread), ARW black/white levels (named in DngRaw.unapplied: arw_black_level_unread, arw_white_level_unread; white is the tone curve maximum), ARW1 and lossless-JPEG ARW (unsupported_compression) and ORF, RW2, RAF, CR3 (recognised by magic: decoder_unavailable naming the family); CR2 camera colour, MakerNote black/white levels, sensor borders and a verified CFA phase (named in DngRaw.unapplied: cr2_black_level_unread, cr2_white_level_unread, cr2_sensor_borders_unread, cr2_cfa_pattern_unverified)"]}"#;
