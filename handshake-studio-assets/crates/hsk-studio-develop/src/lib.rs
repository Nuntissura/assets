//! RAW decode/develop recipes; originals immutable through CKC/ArtifactService provider identity,
//! target-independent scalar oracle and SIMD dispatch. Imported catalog semantics adapt into
//! existing CKC owner, not a private Studio catalog.
//!
//! Slice 1: native process 1.0 / engine 1.0 (`native_cfa_bilinear_hermite_v1`) over caller-granted
//! decoded camera CFA bytes (STU-CON-042..045). Scalar oracle only; no SIMD, no unsafe.
#![forbid(unsafe_code)]

pub mod curves;
pub mod pipeline;
pub mod recipe;

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{FailureCode, Observation, Observe, Outcome, SinkPort};

pub use curves::{Curve, PreparedCurve};
pub use pipeline::{
    Bayer, Cfa, CfaPattern, CfaPlane, DevelopReceipt, DevelopRequest, DevelopedTile,
    ExecutionHint, Limits, PlaneInit, SinkError, demosaic_normalized, develop,
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
    UnsupportedContribution(&'static str),
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
            Self::UnsupportedContribution(_) => "unsupported_contribution",
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
    pub const fn outcome(self) -> Outcome {
        match self {
            Self::Canceled => Outcome::Canceled,
            Self::UnsupportedProcessVersion
            | Self::UnsupportedMathToken
            | Self::UnsupportedDemosaic
            | Self::UnsupportedWhiteBalance
            | Self::UnsupportedCrop
            | Self::UnsupportedProfileTransform
            | Self::UnsupportedExecution
            | Self::UnsupportedPattern
            | Self::UnsupportedCurve
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

/// Explicit "missing RAW container decoder" outcome (STU-CON-042: direct container input and
/// unresolved decoder capability return typed refusal). DNG decode lands in the `dng` module.
pub fn decode_container(_bytes: &[u8]) -> Result<CfaPlane, DevelopError> {
    Err(DevelopError::DecoderUnavailable)
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

pub const DESCRIPTOR: &str = r#"{"crate":"hsk-studio-develop","module":"develop","slice":1,"scope":"source-only decoded-CFA develop over caller-granted bytes; no RAW container decoder, no catalog, no persistence, no host wiring","process_version":"1.0","engine_version":"1.0","math_token":"native_cfa_bilinear_hermite_v1","demosaic_algorithm":"bilinear_reflect_native_v1","cfa":["RGGB","BGGR","GRBG","GBRG","xtrans_6x6"],"white_balance_modes":["as_shot"],"profile_transform":"identity_only_working_equals_output","curves":"monotone_hermite_2_to_32_points_linear_output_domain","execution":{"supported":["scalar","auto"],"auto_route":"scalar","unsupported":["sse2"]},"limits":{"spec_v1":{"side_min_bayer":2,"side_min_xtrans":6,"side_max":256,"stride_max_bytes":1024,"plane_max_bytes":262144,"tile_edge_max":64,"tiles_max":16,"output_max_bytes":16777216}},"output":{"colour":"rgb_f32le linear_light","alpha":"f32le straight, 1.0 inside crop only","coverage":"coverage_u8"},"supported_enables":["enable_tone_curve"],"refusals":"typed, stable snake_case codes; nothing clipped, defaulted or silently zeroed","pending":["SIMD route","prism profile transform","host grants and publication","RAW container decoders other than DNG"]}"#;
