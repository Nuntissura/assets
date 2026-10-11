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

pub mod color;
pub mod convert;
pub mod curves;
pub mod dng;
pub mod pipeline;
pub mod recipe;
pub mod ljpeg;
mod par;
mod tiff;

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{FailureCode, Observation, Observe, Outcome, SinkPort};

pub use color::{CameraSolution, DngColor};
pub use convert::{
    BandLimits, ConversionJob, ConversionStats, IccProfile, PrismContext, PrismConverter,
    ProfileConverter, WIDE_CHUNK_PIXELS,
};
pub use curves::{Curve, PreparedCurve};
pub use dng::{ActiveArea, BaselineExposure, DngRaw, PlaneWindow, RangePolicy, WindowReport};
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
    DecoderUnavailable,
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
            Self::DecoderUnavailable => "decoder_unavailable",
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
            Self::DecoderUnavailable | Self::SinkRejected => {
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

/// Container entry point. DNG (TIFF header + DNGVersion) decodes to an immutable raw frame; any
/// other container is the explicit "missing RAW decoder" outcome (STU-CON-042: typed refusal).
pub fn decode_container(bytes: &[u8]) -> Result<DngRaw, DevelopError> {
    // TIFF family (classic + BigTIFF magic); BigTIFF is then refused by the reader, not mislabeled.
    if [&b"II*\0"[..], b"MM\0*", b"II+\0", b"MM\0+"]
        .iter()
        .any(|magic| bytes.starts_with(magic))
    {
        dng::decode(bytes)
    } else {
        Err(DevelopError::DecoderUnavailable)
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

pub const DESCRIPTOR: &str = r#"{"crate":"hsk-studio-develop","module":"develop","slice":1,"scope":"source-only decoded-CFA develop over caller-granted bytes plus an own DNG reader (classic TIFF, uncompressed 8/16-bit or lossless JPEG, strips or tiles, ColorMatrix/ForwardMatrix); no catalog, no persistence, no host wiring","process_version":"1.0","engine_version":"1.0","math_token":"native_cfa_bilinear_hermite_v1","demosaic_algorithm":"bilinear_reflect_native_v1","cfa":["RGGB","BGGR","GRBG","GBRG","xtrans_6x6"],"white_balance_modes":["as_shot"],"profile_transform":{"identity":"working equals output (inspected when a converter is supplied)","prism_admitted_wide":"develop_with + PrismConverter; both profiles inspected through the admitted Prism route (linear-light RGB profiles only), pixels converted by a Prism AdmittedWideEngine job (prepare_job; one band per tile, a tile above 262144 pixels is several bands, limit 2097152 per band and 65536 bands per job) that the tile workers share and convert in place between stage A and stage C (the plan is resolved once per job; each profile is parsed once per cached plan; receipt profile_stats = plan hits/misses/parses per job, profile_calls = bands); working RGB must be finite in 0..1 else prism refuses (never clipped); relative colorimetric, BPC off; output unclamped, destination values outside 0..1 counted in profile_stats.clip; exposure and curves run after conversion; every wide-path allocation (engine, then at prepare: work, receipt, plan and transient build leases; band conversion allocates nothing) is charged to the caller admission ledger first and a denial is prism_refused:admission_denied before any parse (tests/develop_profile.rs: short_admission_budget_is_refused_before_any_work)"},"curves":"monotone_hermite_2_to_32_points_linear_output_domain","execution":{"supported":["scalar","auto"],"auto_route":"scalar","unsupported":["sse2"],"threads":{"api":"develop_with_threads / develop_to_image_threads / demosaic_normalized_threads (Threads)","default":"min(available_parallelism, 16)","max":64,"unit":"normalisation row bands and output tiles (std scoped threads, no unsafe)","deterministic":"tiles, receipt and refusals are identical for every thread count; the lowest failing tile decides the error; cancellation between bands and tiles returns canceled with no partial output (tests/develop_parallel.rs: output_is_identical_for_every_thread_count, lowest_failing_tile_decides_the_error_for_every_thread_count, cancellation_returns_canceled_and_no_partial_output)","converter":"one converter job per window of tiles is prepared before any tile is computed; each worker converts its own tile bands in place between stage A and stage C and the job is finished once on the calling thread (tests/develop_parallel.rs: converter_bands_run_once_each_on_the_tile_workers)"}},"limits":{"spec_v1":{"side_min_bayer":2,"side_min_xtrans":6,"side_max":256,"stride_max_bytes":1024,"plane_max_bytes":262144,"tile_edge_max":64,"tiles_max":16,"output_max_bytes":16777216}},"output":{"colour":"rgb_f32le linear_light","alpha":"f32le straight, 1.0 inside crop only","coverage":"coverage_u8"},"supported_enables":["enable_tone_curve"],"refusals":"typed, stable snake_case codes; nothing clipped, defaulted or silently zeroed","pending":["spec STU-CON-044 text still names 64-pixel chunks (the wide route uses bands of up to 262144 pixels; enrichment pending)","SIMD route","non-linear (encoded) profiles and BPC in Prism","host grants and publication","DNG compressions other than none and lossless JPEG (deflate, lossy JPEG)","proprietary RAW containers (decoder_unavailable)"]}"#;
