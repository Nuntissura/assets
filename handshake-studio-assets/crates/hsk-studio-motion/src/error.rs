//! Typed, stable-coded rejections for every Motion operation. No free text, no source content.
use crate::path::PropertyPath;
use hsk_studio_observe::FailureCode;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MotionError {
    Canceled,
    /// Property key segment empty, too long, containing `/` or control characters.
    InvalidKey,
    /// Property path empty, too long, has an empty/`.`/`..` segment or control characters.
    InvalidPath,
    /// Component count 0 or above `MAX_DIMENSION`.
    DimensionLimit,
    KeyframeLimit,
    ExpressionTooLong,
    NonFinite,
    /// Influence outside `0..=1` (or non-finite).
    InvalidInfluence,
    InvalidBounds,
    OutOfBounds,
    DuplicateKeyTime,
    UnsortedKeys,
    ComponentMismatch,
    /// A Bezier-family side has no temporal tangent for a component.
    MissingTangent,
    /// Influence pair makes the segment time curve fold back on itself (x(s) not monotone).
    NonMonotoneTime,
    TickOverflow,
    StaleRevision {
        expected: u64,
        actual: u64,
    },
    /// Dependency cycle: participants in cycle order starting at the committing property.
    Cycle {
        participants: Vec<PropertyPath>,
    },
    ReferenceMissing {
        reference: String,
    },
    AmbiguousReference {
        reference: String,
        candidates: usize,
    },
    ReadLimit,
    NodeLimit,
    /// Date clock value outside the exact +-8.64e15 ms range (MOT-256).
    DateOutOfRange,
    InvalidDateProfile,
}

impl MotionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Canceled => "canceled",
            Self::InvalidKey => "invalid_key",
            Self::InvalidPath => "invalid_path",
            Self::DimensionLimit => "dimension_limit",
            Self::KeyframeLimit => "keyframe_limit",
            Self::ExpressionTooLong => "expression_too_long",
            Self::NonFinite => "nonfinite",
            Self::InvalidInfluence => "invalid_influence",
            Self::InvalidBounds => "invalid_bounds",
            Self::OutOfBounds => "out_of_bounds",
            Self::DuplicateKeyTime => "duplicate_key_time",
            Self::UnsortedKeys => "unsorted_keys",
            Self::ComponentMismatch => "component_mismatch",
            Self::MissingTangent => "missing_tangent",
            Self::NonMonotoneTime => "non_monotone_time",
            Self::TickOverflow => "tick_overflow",
            Self::StaleRevision { .. } => "stale_revision",
            Self::Cycle { .. } => "dependency_cycle",
            Self::ReferenceMissing { .. } => "reference_missing",
            Self::AmbiguousReference { .. } => "ambiguous_reference",
            Self::ReadLimit => "read_limit",
            Self::NodeLimit => "node_limit",
            Self::DateOutOfRange => "date_out_of_range",
            Self::InvalidDateProfile => "invalid_date_profile",
        }
    }

    /// Closed observe vocabulary mapping; cancellation is reported via `Outcome::Canceled`.
    pub fn failure_code(&self) -> FailureCode {
        FailureCode::Validation
    }
}

impl fmt::Display for MotionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for MotionError {}
