//! Proof records (lane SMB-PS-SOFT-PROOF-2): the per-document soft-proof state `StudioProofRecords`
//! (schema `hsk.studio.proof_records@1`) beside the document like the multicam record set
//! (`StudioDocument` gains no field, so every existing document is byte-identical).
//!
//! Spec basis: STU-COL-240 "Soft-proofing simulates an output condition on screen WITHOUT converting
//! document values. It is toggleable, its state is document-scoped and inspectable, and it never alters
//! authority values"; STU-COL-241 "A proof condition carries SIX fields: `proof_profile` ...,
//! `proof_intent` ..., `display_intent` ..., `black_point_compensation` (bool), `simulate_paper_color`
//! (bool) and `simulate_black_ink` (bool)"; STU-COL-242 "A proof condition is nameable, savable and
//! selectable"; STU-COL-244 parameter table row `gamut_warning.opacity` 0..100, default 100, percent.
//!
//! Stored (data only; the proof chain and the gamut warning are Prism's): `conditions` are the
//! document's saved proof conditions (the preset menu is the shipped presets plus these);
//! `active_condition` names a saved condition or a shipped preset; `proof_colours` is the Proof
//! Colours toggle; `gamut_warning` is the Gamut Warning toggle with its colour and opacity.
//!
//! Studio readings, declared (lane notes `spec_deltas`):
//! - the shipped preset names are Prism's (`SHIPPED_PROOF_PRESETS`, same order as
//!   `hsk_studio_prism::shipped_proof_conditions`); a saved condition may not reuse one, so a name
//!   selects exactly one condition;
//! - an ICC proof profile is referenced by its `SCPF` profile id and the sha256 `content_digest` of
//!   its bytes (the bytes stay outside Folio, like `StudioIccProfile`); Folio checks the shape of the
//!   reference, the host resolves the bytes and reports an unresolved reference;
//! - an operator matrix is the 3x3 linear-light matrix of STU-COL-243 in row-major order with
//!   Prism's bounds (finite, every coefficient `|c| <= 4`, every row sum `0..=2`);
//! - the warning colour is display RGB `0..=1` per channel, default mid grey (Prism's default);
//! - Proof Colours or Gamut Warning on without an active condition is refused (nothing to proof).
//!
//! Refusals: out-of-range values `InvalidField`, ids and names of the wrong shape `InvalidId` /
//! `InvalidField`, duplicate condition names `DuplicateIdentity`, an active condition that names
//! nothing `DanglingReference`, counts beyond the hard ceiling or the caller node budget
//! `BudgetExceeded`. The records arrive from untrusted wire JSON: integer arithmetic is denied
//! (workflow V4-01) and the condition list is counted before it is walked.
#![deny(clippy::arithmetic_side_effects)]
use super::{Budget, Code, ContentDigest, Result, StudioDocument, err, id, values::Ctx};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

schema_token!(ProofRecordsSchema, "hsk.studio.proof_records@1");

/// Saved proof conditions in one record set.
pub const MAX_PROOF_CONDITIONS: usize = 256;
/// Longest condition name in bytes (Prism `MAX_PROOF_NAME_BYTES`).
pub const MAX_PROOF_CONDITION_NAME_BYTES: usize = 128;
/// Shipped preset names, in Prism's order (STU-COL-242 minimum plus the STU-COL-243 conditions).
pub const SHIPPED_PROOF_PRESETS: [&str; 6] =
    ["working_cmyk", "working_rgb", "legacy_uncompensated_rgb", "protanopia", "deuteranopia", "tritanopia"];
/// Bound of every operator matrix coefficient (Prism `OPERATOR_COEFFICIENT_LIMIT`).
pub const PROOF_OPERATOR_COEFFICIENT_LIMIT: f64 = 4.0;
/// Bounds of every operator matrix row sum (Prism `OPERATOR_ROW_SUM_MIN` / `_MAX`).
pub const PROOF_OPERATOR_ROW_SUM_MIN: f64 = 0.0;
pub const PROOF_OPERATOR_ROW_SUM_MAX: f64 = 2.0;
/// STU-COL-244 parameter table: `gamut_warning.opacity` 0..=100 percent, default 100.
pub const PROOF_GAMUT_OPACITY_MIN: u32 = 0;
pub const PROOF_GAMUT_OPACITY_MAX: u32 = 100;
pub const PROOF_GAMUT_OPACITY_DEFAULT: u32 = 100;
/// Default warning colour, display RGB (Prism `DEFAULT_GAMUT_WARNING_COLOUR`).
pub const PROOF_DEFAULT_WARNING_COLOUR: [f64; 3] = [0.5, 0.5, 0.5];

/// A shipped proof profile a saved condition can use with its own intents and flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShippedProofProfile {
    /// The current CMYK working space.
    WorkingCmyk,
    /// The current RGB working space.
    WorkingRgb,
    /// Legacy uncompensated RGB (document RGB values shown unmanaged).
    LegacyUncompensatedRgb,
}

/// STU-COL-243 colour-vision proof conditions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProofColourVision {
    Protanopia,
    Deuteranopia,
    Tritanopia,
}

/// Rendering intent of one proof stage (STU-COL-241 `proof_intent` / `display_intent`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioProofIntent {
    Perceptual,
    RelativeColorimetric,
    Saturation,
    AbsoluteColorimetric,
}

/// STU-COL-241 `proof_profile`: the device being simulated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioProofProfile {
    Shipped {
        preset: ShippedProofProfile,
    },
    /// An ICC device profile by `SCPF` id and the sha256 of its bytes (module docs).
    Icc {
        profile_id: String,
        content_digest: ContentDigest,
    },
    ColourVision {
        vision: ProofColourVision,
    },
    /// Operator-supplied linear-light 3x3 matrix, row-major.
    OperatorMatrix {
        matrix: [f64; 9],
    },
}

/// One saved proof condition: a name and the six STU-COL-241 fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioProofConditionRecord {
    pub name: String,
    pub proof_profile: StudioProofProfile,
    pub proof_intent: StudioProofIntent,
    pub display_intent: StudioProofIntent,
    pub black_point_compensation: bool,
    pub simulate_paper_color: bool,
    pub simulate_black_ink: bool,
}

fn default_warning_colour() -> [f64; 3] {
    PROOF_DEFAULT_WARNING_COLOUR
}
fn default_warning_opacity() -> u32 {
    PROOF_GAMUT_OPACITY_DEFAULT
}

/// The Gamut Warning toggle with its configurable colour and opacity (STU-COL-244).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGamutWarningState {
    pub enabled: bool,
    /// Display RGB, each channel 0..=1.
    #[serde(default = "default_warning_colour")]
    pub colour: [f64; 3],
    /// Percent, 0..=100.
    #[serde(default = "default_warning_opacity")]
    pub opacity: u32,
}
impl Default for StudioGamutWarningState {
    fn default() -> Self {
        Self { enabled: false, colour: PROOF_DEFAULT_WARNING_COLOUR, opacity: PROOF_GAMUT_OPACITY_DEFAULT }
    }
}

/// The per-document proof record set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioProofRecords {
    pub schema_id: ProofRecordsSchema,
    /// `SDOC` id of the annotated document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<StudioProofConditionRecord>,
    /// A saved condition name or a shipped preset name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_condition: Option<String>,
    /// The Proof Colours toggle.
    #[serde(default)]
    pub proof_colours: bool,
    #[serde(default)]
    pub gamut_warning: StudioGamutWarningState,
}
impl StudioProofRecords {
    /// An empty set for `document_id`: nothing saved, nothing active, both toggles off.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self {
            schema_id: ProofRecordsSchema::V1,
            document_id: document_id.into(),
            conditions: Vec::new(),
            active_condition: None,
            proof_colours: false,
            gamut_warning: StudioGamutWarningState::default(),
        }
    }
    /// The saved condition `name`.
    pub fn condition(&self, name: &str) -> Option<&StudioProofConditionRecord> {
        self.conditions.iter().find(|c| c.name == name)
    }
    /// Whether `name` is a shipped preset name.
    pub fn is_shipped(name: &str) -> bool {
        SHIPPED_PROOF_PRESETS.contains(&name)
    }
}

/// JSON schema (draft 2020-12) of the proof record set.
pub fn proof_records_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12().into_generator().into_root_schema_for::<StudioProofRecords>()
}

/// Parses wire JSON (bounded by `Budget::input_bytes` and depth before any parse; duplicate keys
/// refused) and validates it against the document.
pub fn parse_proof_records(
    input: &[u8],
    d: &StudioDocument,
    b: Budget,
    token: &CancellationToken,
) -> Result<StudioProofRecords> {
    super::preflight(input, b, token)?;
    let value = serde_json::from_slice::<super::UniqueValue>(input)
        .map_err(|e| {
            err(
                if e.to_string().contains("duplicate_field") { Code::DuplicateField } else { Code::InvalidJson },
                "proof_records",
            )
        })?
        .0;
    let r: StudioProofRecords = serde_json::from_value(value).map_err(|_| err(Code::InvalidJson, "proof_records"))?;
    validate_proof_records(&r, d, b, token)?;
    Ok(r)
}

/// A condition name: non-empty after trimming, at most `MAX_PROOF_CONDITION_NAME_BYTES`, no control
/// characters (Prism's name rule).
fn name_ok(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= MAX_PROOF_CONDITION_NAME_BYTES && !name.chars().any(char::is_control)
}

/// A `sha256` digest: algorithm `sha256`, 64 lowercase hex digits.
fn sha256(d: &ContentDigest) -> bool {
    d.algorithm == "sha256" && d.digest.len() == 64 && d.digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Prism's operator matrix bounds (module docs).
fn matrix_ok(m: &[f64; 9]) -> bool {
    m.iter().all(|v| v.is_finite() && v.abs() <= PROOF_OPERATOR_COEFFICIENT_LIMIT)
        && m.chunks_exact(3)
            .map(|row| row.iter().sum::<f64>())
            .all(|s| (PROOF_OPERATOR_ROW_SUM_MIN..=PROOF_OPERATOR_ROW_SUM_MAX).contains(&s))
}

/// Validates the record set against the document (module docs).
pub fn validate_proof_records(r: &StudioProofRecords, d: &StudioDocument, b: Budget, token: &CancellationToken) -> Result<()> {
    let mut cx = Ctx::new(b, token, &super::Unresolved);
    cx.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if r.conditions.len() > MAX_PROOF_CONDITIONS {
        return Err(err(Code::BudgetExceeded, "conditions"));
    }
    cx.nodes(r.conditions.len(), "conditions")?;
    let mut names = BTreeSet::new();
    for (i, c) in r.conditions.iter().enumerate() {
        cx.tick()?;
        let t = format!("conditions/{i}");
        if !name_ok(&c.name) || StudioProofRecords::is_shipped(&c.name) {
            return Err(err(Code::InvalidField, format!("{t}/name")));
        }
        if !names.insert(c.name.as_str()) {
            return Err(err(Code::DuplicateIdentity, format!("{t}/name")));
        }
        match &c.proof_profile {
            StudioProofProfile::Shipped { .. } | StudioProofProfile::ColourVision { .. } => {}
            StudioProofProfile::Icc { profile_id, content_digest } => {
                id(profile_id, "SCPF", &format!("{t}/proof_profile/profile_id"))?;
                if !sha256(content_digest) {
                    return Err(err(Code::InvalidField, format!("{t}/proof_profile/content_digest")));
                }
            }
            StudioProofProfile::OperatorMatrix { matrix } => {
                if !matrix_ok(matrix) {
                    return Err(err(Code::InvalidField, format!("{t}/proof_profile/matrix")));
                }
            }
        }
    }
    if let Some(active) = &r.active_condition
        && !names.contains(active.as_str())
        && !StudioProofRecords::is_shipped(active)
    {
        return Err(err(Code::DanglingReference, "active_condition"));
    }
    let g = &r.gamut_warning;
    if g.colour.iter().any(|c| !c.is_finite() || !(0.0..=1.0).contains(c)) {
        return Err(err(Code::InvalidField, "gamut_warning/colour"));
    }
    if !(PROOF_GAMUT_OPACITY_MIN..=PROOF_GAMUT_OPACITY_MAX).contains(&g.opacity) {
        return Err(err(Code::InvalidField, "gamut_warning/opacity"));
    }
    if r.active_condition.is_none() {
        if r.proof_colours {
            return Err(err(Code::InvalidField, "proof_colours"));
        }
        if g.enabled {
            return Err(err(Code::InvalidField, "gamut_warning/enabled"));
        }
    }
    Ok(())
}
