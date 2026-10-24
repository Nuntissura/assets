//! Soft proofing (SMB-PS-SOFT-PROOF-1; STU-COL-022, STU-COL-023, STU-COL-240..245, STU-COL-138).
//!
//! A proof simulates an output condition on screen WITHOUT converting document values: the document pixels
//! are borrowed immutably and every result is a new buffer. The condition record (`condition.rs`) carries the
//! six STU-COL-241 fields; the chain (`chain.rs`) runs document -> proof device (`proof_intent`, black point
//! compensation) -> display (`display_intent`) on the wide-path stages (`icc_eval.rs`, `bpc.rs`); colour-vision
//! conditions (`cvd.rs`) use the same chain with a linear-light sRGB proof device and a published simulation
//! matrix; the gamut warning and the in-gamut substitute live in `gamut.rs`.
#![deny(clippy::arithmetic_side_effects)] // V4-01: hostile sizes, explicit checked/saturating/wrapping math
use crate::WideError;

mod condition;
pub use condition::*;
mod chain;
pub use chain::{DISPLAY_BPC_RULE_BLACK_INK_SIMULATED, DisplayStageReport, ProofPreview, ProofReceipt, SoftProof, prepare_soft_proof};
mod cvd;
pub use cvd::{CVD_PROOF_DEVICE_PROFILE_ID, MACHADO_2009_DEUTERANOPIA, MACHADO_2009_PROTANOPIA, MACHADO_2009_TRITANOPIA};

/// Typed refusals of the soft-proof API. Codes are stable snake_case strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofError {
    /// A profile, plan or pixel refusal of the wide path (admission runs size and hash checks before parsing).
    Wide(WideError),
    NameEmpty,
    NameTooLong,
    NameControl,
    OperatorMatrixNonfinite,
    OperatorCoefficientRange,
    OperatorRowSumRange,
    /// The display profile is not an RGB device.
    DisplayNotRgb,
    /// Legacy uncompensated RGB shows RGB document values unmanaged; the document is not RGB.
    LegacyNeedsRgbDocument,
    /// `width` is 0 or does not divide the pixel count.
    Geometry,
    /// Gamut warning opacity outside 0..=100 percent (STU-COL-244 parameter table).
    OpacityRange,
    /// Gamut warning colour channel not finite or outside 0..1.
    WarningColourRange,
}
impl ProofError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Wide(e) => e.code(),
            Self::NameEmpty => "name_empty",
            Self::NameTooLong => "name_too_long",
            Self::NameControl => "name_control",
            Self::OperatorMatrixNonfinite => "operator_matrix_nonfinite",
            Self::OperatorCoefficientRange => "operator_coefficient_range",
            Self::OperatorRowSumRange => "operator_row_sum_range",
            Self::DisplayNotRgb => "display_not_rgb",
            Self::LegacyNeedsRgbDocument => "legacy_needs_rgb_document",
            Self::Geometry => "geometry",
            Self::OpacityRange => "opacity_range",
            Self::WarningColourRange => "warning_colour_range",
        }
    }
}
impl std::fmt::Display for ProofError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ProofError {}
impl From<WideError> for ProofError {
    fn from(e: WideError) -> Self {
        Self::Wide(e)
    }
}
