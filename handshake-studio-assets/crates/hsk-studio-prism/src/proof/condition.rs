//! Proof condition record (STU-COL-241) and the shipped presets (STU-COL-242, STU-COL-243).
//!
//! A condition carries the six spec fields as six independent public fields: `proof_profile`,
//! `proof_intent` (document -> proof device), `display_intent` (proof device -> display),
//! `black_point_compensation`, `simulate_paper_color` and `simulate_black_ink`. Nothing derives one
//! field from another (tests/soft_proof.rs `proof_condition_has_six_independent_fields`).
#![deny(clippy::arithmetic_side_effects)] // V4-01: hostile sizes, explicit checked/saturating/wrapping math
use super::ProofError;
use crate::{Intent, ProfileInput, hash_hex};
use hsk_studio_accord::DomainId;

/// Longest condition name, in bytes.
pub const MAX_PROOF_NAME_BYTES: usize = 128;
/// Bound of every coefficient of an operator-supplied linear-light simulation matrix (Studio reading;
/// the spec sets no bound): `|m[r][c]| <= 4`.
pub const OPERATOR_COEFFICIENT_LIMIT: f64 = 4.0;
/// Bounds of every row sum of an operator-supplied matrix (Studio reading): a row sum of 1 keeps the
/// neutral axis, 0..=2 admits darkening and brightening simulations but no sign flip of white.
pub const OPERATOR_ROW_SUM_MIN: f64 = 0.0;
pub const OPERATOR_ROW_SUM_MAX: f64 = 2.0;

/// Preset names of the shipped conditions (STU-COL-242 minimum plus STU-COL-243).
pub const PRESET_WORKING_CMYK: &str = "working_cmyk";
pub const PRESET_WORKING_RGB: &str = "working_rgb";
pub const PRESET_LEGACY_UNCOMPENSATED_RGB: &str = "legacy_uncompensated_rgb";
pub const PRESET_PROTANOPIA: &str = "protanopia";
pub const PRESET_DEUTERANOPIA: &str = "deuteranopia";
pub const PRESET_TRITANOPIA: &str = "tritanopia";

/// Colour-vision-deficiency proof profiles (STU-COL-243): dichromat simulations of Machado,
/// Oliveira and Fernandes 2009 at severity 1.0 (matrices in `cvd.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColourVision {
    Protanopia,
    Deuteranopia,
    Tritanopia,
}
impl ColourVision {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Protanopia => "protanopia",
            Self::Deuteranopia => "deuteranopia",
            Self::Tritanopia => "tritanopia",
        }
    }
}

/// Operator-supplied colour-vision condition (STU-COL-243 "MUST allow an operator-supplied condition"): a
/// 3x3 matrix applied to linear-light sRGB proof device values, exactly like the shipped CVD matrices. Only
/// `new` constructs it, so every instance is inside the declared bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OperatorSimulation {
    matrix: [[f64; 3]; 3],
}
impl OperatorSimulation {
    /// Every coefficient finite and within `OPERATOR_COEFFICIENT_LIMIT`, every row sum within
    /// `OPERATOR_ROW_SUM_MIN..=OPERATOR_ROW_SUM_MAX` (tests/soft_proof.rs `operator_condition_bounds_are_enforced`).
    pub fn new(matrix: [[f64; 3]; 3]) -> Result<Self, ProofError> {
        if matrix.iter().flatten().any(|v| !v.is_finite()) {
            return Err(ProofError::OperatorMatrixNonfinite);
        }
        if matrix.iter().flatten().any(|v| v.abs() > OPERATOR_COEFFICIENT_LIMIT) {
            return Err(ProofError::OperatorCoefficientRange);
        }
        if matrix
            .iter()
            .map(|row| row[0] + row[1] + row[2])
            .any(|sum| !(OPERATOR_ROW_SUM_MIN..=OPERATOR_ROW_SUM_MAX).contains(&sum))
        {
            return Err(ProofError::OperatorRowSumRange);
        }
        Ok(Self { matrix })
    }
    pub const fn matrix(&self) -> [[f64; 3]; 3] {
        self.matrix
    }
}

/// The device being simulated.
#[derive(Clone, Copy)]
pub enum ProofProfile<'a> {
    /// An ICC device profile through the wide-path admission (Gray, RGB or CMYK; hostile bytes refused before parsing).
    Icc(ProfileInput<'a>),
    /// A shipped colour-vision-deficiency condition on linear-light sRGB proof device values.
    ColourVision(ColourVision),
    /// An operator-supplied linear-light matrix condition, same pipeline as `ColourVision`.
    OperatorSimulation(OperatorSimulation),
    /// Legacy uncompensated RGB (Studio reading, STU-COL-242 defines no meaning): the document's RGB device
    /// values are shown on the display unmanaged; the display stage is the identity.
    LegacyUncompensatedRgb,
}
impl ProofProfile<'_> {
    pub fn kind(&self) -> ProofProfileKind {
        match self {
            Self::Icc(_) => ProofProfileKind::Icc,
            Self::ColourVision(k) => ProofProfileKind::ColourVision(*k),
            Self::OperatorSimulation(_) => ProofProfileKind::OperatorSimulation,
            Self::LegacyUncompensatedRgb => ProofProfileKind::LegacyUncompensatedRgb,
        }
    }
}
impl std::fmt::Debug for ProofProfile<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Icc(p) => write!(f, "Icc({}, sha256 {})", p.profile_id.as_str(), hash_hex(&p.expected_sha256)),
            Self::ColourVision(k) => write!(f, "ColourVision({})", k.code()),
            Self::OperatorSimulation(m) => write!(f, "OperatorSimulation({:?})", m.matrix),
            Self::LegacyUncompensatedRgb => f.write_str("LegacyUncompensatedRgb"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofProfileKind {
    Icc,
    ColourVision(ColourVision),
    OperatorSimulation,
    LegacyUncompensatedRgb,
}
impl ProofProfileKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Icc => "icc",
            Self::ColourVision(k) => k.code(),
            Self::OperatorSimulation => "operator_simulation",
            Self::LegacyUncompensatedRgb => "legacy_uncompensated_rgb",
        }
    }
}

/// A nameable, savable, selectable proof condition (STU-COL-241/242).
#[derive(Clone, Debug)]
pub struct ProofCondition<'a> {
    pub name: String,
    pub proof_profile: ProofProfile<'a>,
    /// Document -> proof device.
    pub proof_intent: Intent,
    /// Proof device -> display.
    pub display_intent: Intent,
    /// Black point compensation of the document -> proof device stage.
    pub black_point_compensation: bool,
    /// Proof paper white shown (display stage: absolute media-white scaling, Studio reading).
    pub simulate_paper_color: bool,
    /// Proof ink black shown (display stage: no black point compensation, Studio reading).
    pub simulate_black_ink: bool,
}
impl<'a> ProofCondition<'a> {
    /// A condition with Studio's defaults: relative colorimetric on both stages, black point compensation on,
    /// paper and black ink simulation off (Studio reading; Photoshop defaults UNVERIFIED).
    pub fn new(name: &str, proof_profile: ProofProfile<'a>) -> Self {
        Self {
            name: name.to_string(),
            proof_profile,
            proof_intent: Intent::RelativeColorimetric,
            display_intent: Intent::RelativeColorimetric,
            black_point_compensation: true,
            simulate_paper_color: false,
            simulate_black_ink: false,
        }
    }
    /// Shipped preset: the current CMYK working space (the caller passes the working CMYK profile).
    pub fn working_cmyk(profile: ProfileInput<'a>) -> Self {
        Self::new(PRESET_WORKING_CMYK, ProofProfile::Icc(profile))
    }
    /// Shipped preset: the current RGB working space (the caller passes the working RGB profile).
    pub fn working_rgb(profile: ProfileInput<'a>) -> Self {
        Self::new(PRESET_WORKING_RGB, ProofProfile::Icc(profile))
    }
    /// Shipped preset: legacy uncompensated RGB (document RGB values shown unmanaged).
    pub fn legacy_uncompensated_rgb() -> Self {
        Self::new(PRESET_LEGACY_UNCOMPENSATED_RGB, ProofProfile::LegacyUncompensatedRgb)
    }
    /// Shipped colour-vision presets.
    pub fn colour_vision(kind: ColourVision) -> Self {
        Self::new(kind.code(), ProofProfile::ColourVision(kind))
    }
    /// Operator-supplied condition.
    pub fn operator(name: &str, simulation: OperatorSimulation) -> Self {
        Self::new(name, ProofProfile::OperatorSimulation(simulation))
    }
    /// Name checks (non-empty, at most `MAX_PROOF_NAME_BYTES`, no control characters). Profile bytes are admitted
    /// by `prepare_soft_proof`, size and hash first (tests/soft_proof.rs `hostile_proof_profiles_are_refused_before_allocation`).
    pub fn validate(&self) -> Result<(), ProofError> {
        if self.name.trim().is_empty() {
            return Err(ProofError::NameEmpty);
        }
        if self.name.len() > MAX_PROOF_NAME_BYTES {
            return Err(ProofError::NameTooLong);
        }
        if self.name.chars().any(char::is_control) {
            return Err(ProofError::NameControl);
        }
        Ok(())
    }
    /// Owned, savable record of the condition (profile bytes are referenced by id and SHA-256, never copied).
    pub fn record(&self) -> ProofConditionRecord {
        let (profile_id, profile_sha256) = match self.proof_profile {
            ProofProfile::Icc(p) => (Some(p.profile_id.clone()), Some(p.expected_sha256)),
            _ => (None, None),
        };
        ProofConditionRecord {
            name: self.name.clone(),
            kind: self.proof_profile.kind(),
            profile_id,
            profile_sha256,
            operator_matrix: match self.proof_profile {
                ProofProfile::OperatorSimulation(m) => Some(m.matrix()),
                _ => None,
            },
            proof_intent: self.proof_intent,
            display_intent: self.display_intent,
            black_point_compensation: self.black_point_compensation,
            simulate_paper_color: self.simulate_paper_color,
            simulate_black_ink: self.simulate_black_ink,
        }
    }
}

/// The shipped presets in a fixed order: working CMYK, working RGB, legacy uncompensated RGB, protanopia,
/// deuteranopia, tritanopia (tests/soft_proof.rs `presets_include_the_spec_minimum`).
pub fn shipped_proof_conditions<'a>(working_cmyk: ProfileInput<'a>, working_rgb: ProfileInput<'a>) -> Vec<ProofCondition<'a>> {
    vec![
        ProofCondition::working_cmyk(working_cmyk),
        ProofCondition::working_rgb(working_rgb),
        ProofCondition::legacy_uncompensated_rgb(),
        ProofCondition::colour_vision(ColourVision::Protanopia),
        ProofCondition::colour_vision(ColourVision::Deuteranopia),
        ProofCondition::colour_vision(ColourVision::Tritanopia),
    ]
}

/// Select a condition by name (exact match).
pub fn select_proof_condition<'c, 'a>(conditions: &'c [ProofCondition<'a>], name: &str) -> Option<&'c ProofCondition<'a>> {
    conditions.iter().find(|c| c.name == name)
}

/// Owned record of a condition: what a document saves and what every proof receipt names.
#[derive(Clone, Debug, PartialEq)]
pub struct ProofConditionRecord {
    pub name: String,
    pub kind: ProofProfileKind,
    pub profile_id: Option<DomainId>,
    pub profile_sha256: Option<[u8; 32]>,
    pub operator_matrix: Option<[[f64; 3]; 3]>,
    pub proof_intent: Intent,
    pub display_intent: Intent,
    pub black_point_compensation: bool,
    pub simulate_paper_color: bool,
    pub simulate_black_ink: bool,
}
impl ProofConditionRecord {
    /// Deterministic JSON of the record (inspection state; the name is JSON-escaped).
    pub fn to_json(&self) -> String {
        let mut name = String::with_capacity(self.name.len());
        for ch in self.name.chars() {
            match ch {
                '"' => name.push_str("\\\""),
                '\\' => name.push_str("\\\\"),
                c if c.is_control() => name.push_str(&format!("\\u{:04x}", u32::from(c))),
                c => name.push(c),
            }
        }
        let opt = |s: Option<String>| s.map_or_else(|| "null".to_string(), |v| format!("\"{v}\""));
        let matrix = self.operator_matrix.map_or_else(
            || "null".to_string(),
            |m| format!("[[{},{},{}],[{},{},{}],[{},{},{}]]", m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2]),
        );
        format!(
            "{{\"name\":\"{name}\",\"kind\":\"{}\",\"proof_profile_id\":{},\"proof_profile_sha256\":{},\"operator_matrix\":{matrix},\"proof_intent\":\"{}\",\"display_intent\":\"{}\",\"black_point_compensation\":{},\"simulate_paper_color\":{},\"simulate_black_ink\":{}}}",
            self.kind.code(),
            opt(self.profile_id.as_ref().map(|i| i.as_str().to_string())),
            opt(self.profile_sha256.as_ref().map(hash_hex)),
            self.proof_intent.code(),
            self.display_intent.code(),
            self.black_point_compensation,
            self.simulate_paper_color,
            self.simulate_black_ink,
        )
    }
}
