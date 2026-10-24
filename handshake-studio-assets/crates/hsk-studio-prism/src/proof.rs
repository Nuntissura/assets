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
mod gamut;
pub use gamut::{
    DEFAULT_GAMUT_WARNING_COLOUR, GAMUT_LINEAR_TOLERANCE, GAMUT_ROUND_TRIP_DELTA_E76, GAMUT_WARNING_OPACITY_DEFAULT,
    GAMUT_WARNING_OPACITY_MAX, GAMUT_WARNING_OPACITY_MIN, GamutMethod, GamutRegion, GamutSubstitute, GamutWarning,
    GamutWarningStyle, MAX_GAMUT_REGIONS,
};

/// Same descriptor for models, the manual and Argus adapters: the soft-proof API (lib.rs `DESCRIPTOR` points here).
pub const SOFT_PROOF_DESCRIPTOR: &str = r##"{"module":"STUDIO-MODULE-PRISM","path":"soft_proof","descriptor_version":1,"spec":["STU-COL-022","STU-COL-023","STU-COL-138","STU-COL-240","STU-COL-241","STU-COL-242","STU-COL-243","STU-COL-244","STU-COL-245"],"api":"prepare_soft_proof(document ProfileInput, &ProofCondition, display ProfileInput, &CancellationToken) -> SoftProof (Send + Sync) | ProofError; SoftProof::run(&pixels, width, enabled, &cancel) -> ProofPreview {pixels: display RGB f32 interleaved, receipt: ProofReceipt}; SoftProof::gamut_warning(&pixels, width, GamutWarningStyle, &cancel) -> GamutWarning; GamutWarning::apply_overlay(&display_rgb); SoftProof::in_gamut_substitute(&colour, &cancel) -> GamutSubstitute (explicit call only, never automatic)","condition":"ProofCondition {name, proof_profile: ProofProfile::Icc(ProfileInput) | ColourVision(Protanopia|Deuteranopia|Tritanopia) | OperatorSimulation(OperatorSimulation::new(3x3)) | LegacyUncompensatedRgb, proof_intent, display_intent, black_point_compensation, simulate_paper_color, simulate_black_ink}: six independent fields (tests/soft_proof.rs proof_condition_has_six_independent_fields); record() -> ProofConditionRecord with to_json(); presets shipped_proof_conditions(working_cmyk, working_rgb) = working_cmyk, working_rgb, legacy_uncompensated_rgb, protanopia, deuteranopia, tritanopia; select_proof_condition(&conditions, name)","chain":"document -> proof device (proof_intent, condition BPC ISO 18619; matrix/Gray proof devices clamped to 0..1, tables clamp themselves) -> [colour-vision/operator: matrix on linear-light sRGB proof device values, clamped] -> display (display_intent; simulate_black_ink: no display BPC, else ISO 18619 proof black -> display black; simulate_paper_color: absolute colorimetric display stage, PCS scaled by proof wtpt / display wtpt after the black map); toggled off (enabled=false): the plain document -> display wide transform (display_intent, BPC on, fused) bit for bit (tests/soft_proof.rs toggle_off_equals_the_plain_display_transform); the document buffer is borrowed immutably (soft_proof_never_changes_document_values); two intents composed bit for bit from two wide transforms (two_intents_are_applied_separately); deterministic for every thread count (proof_is_deterministic_across_threads)","colour_vision":"Machado, Oliveira, Fernandes 2009 severity 1.0 matrices (MACHADO_2009_*), applied to linear-light sRGB (Studio reading); proof device = WORKING_PROFILE_LINEAR_SRGB under CVD_PROOF_DEVICE_PROFILE_ID; operator matrices: finite, |coefficient| <= 4, row sums 0..=2 (operator_condition_bounds_are_enforced)","gamut":"mask, out_of_gamut_pixels, area_fraction, bounds, regions (8-connected, at most 256, region_count, regions_truncated), max_delta_e76; matrix/Gray proof devices: linear values outside 0..1 by more than 0.001 (the ClipReport test); table proof devices: PCS round trip CIE76 > 5 (Studio reading, LittleCMS cross-reference); perceptual/saturation proof intents checked under relative colorimetric; legacy: not_applicable; style: colour display RGB 0..1 default (0.5,0.5,0.5), gamut_warning.opacity integer 0..=100 default 100","substitute":"document colour rendered by the proof device under proof_intent (clamped), back to the document space relative colorimetric without BPC, clamped 0..1; delta_e76 CIE76 D50; substitute_in_gamut re-checked","receipt":"condition record (both intents, both booleans), document/proof/display ids and SHA-256, proof_bpc, proof_absolute, display_stage {intent, bpc, paper_white_scale}, simulation_matrix, proof_clip, unmanaged, pixel_count, width, height, engine name/version, output_sha256","bounds":{"pixels_per_call":2097152,"profile_bytes":2097152,"name_bytes":128,"gamut_regions":256},"reject":"ProofError::Wide(admission, plan and pixel refusals of the wide path: identity, size and SHA-256 before any parsing) | name_empty | name_too_long | name_control | operator_matrix_nonfinite | operator_coefficient_range | operator_row_sum_range | display_not_rgb | legacy_needs_rgb_document | geometry | opacity_range | warning_colour_range","not_in_scope":["document-scoped proof state record (Folio follow-up)","on-screen toggle, preset menu and overlay in the window (harness-view follow-up)","Photoshop and LittleCMS proof parity (NOT_PROVEN)"],"manual_route":"same descriptor","argus_route":"same descriptor plus ProofReceipt and GamutWarning"}"##;

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
