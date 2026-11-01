//! Ink manager, mixed inks and mixed-ink groups (lane SMB-ID-PREPRESS; STU-LAY-160 ink contract).
//!
//! An [`InkManager`] lists the inks a separation can print: the four process inks and any number of
//! spot inks, each with the captured ink parameters (`ink_angle` 0..=360 deg, `ink_frequency`
//! 1..=500, `neutral_density` 0.001..=10, `solidity` 0..=1, `ink_type` normal / opaque / transparent
//! / opaque_ignore). The spec declares no defaults for them, so a parameter is `None` until stated
//! (not filled from its range). Spot inks may be aliased to another ink (their content prints on
//! that ink's plate), converted to process (per ink or all at once, through the CMYK alternate at full
//! tint), and the use-standard-Lab flag is stored. Captured typed errors: a process ink cannot be
//! aliased; a mixed ink holds at most 27 spot inks and at most 31 inks in total; a mixed-ink group
//! generates at most 1000 swatches (checked with checked arithmetic BEFORE any swatch is built).
//!
//! A mixed-ink group follows the captured definition: per ink an initial tint, a repeat count and an
//! increment; the group's swatches are every combination (ink order = row order, the first row
//! varying slowest). A tint stepping past 100 % is refused (`tint_out_of_range`). How InDesign names
//! and orders generated swatches, and whether it clamps or refuses past 100 %, is UNVERIFIED (D5).
//!
//! Ink manager contract (lane SMB-PRESS-INK-MANAGER-1, Master Spec v02.229 STU-LAY-160 with
//! STU-LAY-100 and STU-LAY-102): the four tabled ink parameters are seven-field records
//! ([`InkParameter::spec`], reusing the coverage contract's [`ParamValue`] / [`ParamPrecision`]);
//! every field the table marks `unknown` stays [`ParamValue::Unknown`] and is never mirrored from the
//! other range. A value is admitted only inside its hard bounds and, for `neutral_density`, at the
//! declared precision of 3 decimal places ([`InkParameter::check`]; a validation error, never a
//! clamp or a rounding). `ink_type` is the one four-member [`InkType`] enumeration the separations
//! and coverage paths read. Editing the manager is a mutating document edit:
//! [`InkManager::apply_edit`] takes the caller's [`ActorContext`] (never a default actor), validates
//! the edited manager as a whole and returns it with an attributed [`InkEditReceipt`], leaving the
//! input unchanged (no partial edit). Mixed inks and mixed-ink groups are represented only in this
//! module ([`MixedInk`], [`MixedInkGroup`]; Folio stores spot inks, not mixed inks), so their
//! captured limits are enforced here. Vendor parity is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use crate::coverage::{Emission, ObserveRoute, ParamPrecision, ParamValue};
use hsk_studio_accord::ActorContext;
use hsk_studio_observe::{DiagnosticCode, Disposition, FailureCode, Outcome};

/// One of the four process inks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProcessInk {
    Cyan,
    Magenta,
    Yellow,
    Black,
}
impl ProcessInk {
    pub const ALL: [ProcessInk; 4] = [Self::Cyan, Self::Magenta, Self::Yellow, Self::Black];
    /// The ink name a separation lists.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cyan => "Process Cyan",
            Self::Magenta => "Process Magenta",
            Self::Yellow => "Process Yellow",
            Self::Black => "Process Black",
        }
    }
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Cyan => 0,
            Self::Magenta => 1,
            Self::Yellow => 2,
            Self::Black => 3,
        }
    }
}

/// STU-LAY-160 `ink_type`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InkType {
    #[default]
    Normal,
    Opaque,
    Transparent,
    OpaqueIgnore,
}
impl InkType {
    pub const ALL: [InkType; 4] = [Self::Normal, Self::Opaque, Self::Transparent, Self::OpaqueIgnore];
    /// STU-LAY-252: opaque and opaque-ignore inks obscure what lies beneath them, so their
    /// contribution to a total ink figure is not a plain sum (see the separations total).
    pub const fn obscures(self) -> bool {
        matches!(self, Self::Opaque | Self::OpaqueIgnore)
    }
    pub const fn code(self) -> &'static str {
        match self {
            Self::Normal => "ink.normal",
            Self::Opaque => "ink.opaque",
            Self::Transparent => "ink.transparent",
            Self::OpaqueIgnore => "ink.opaque_ignore",
        }
    }
    /// The member with exactly this token (STU-LAY-102); anything else is `None`, never a near member.
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.code() == code)
    }
}

/// The four numeric parameters of the STU-LAY-160 ink table, in table order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InkParameter {
    InkAngle,
    InkFrequency,
    NeutralDensity,
    Solidity,
}

/// Units of the STU-LAY-160 ink table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InkUnit {
    Deg,
    Count,
    Ratio,
}
impl InkUnit {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Deg => "deg",
            Self::Count => "count",
            Self::Ratio => "ratio",
        }
    }
}

/// The seven independent fields of one STU-LAY-160 ink parameter (STU-LAY-100).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InkParameterSpec {
    pub parameter: InkParameter,
    pub hard_min: ParamValue,
    pub hard_max: ParamValue,
    pub soft_min: ParamValue,
    pub soft_max: ParamValue,
    pub default: ParamValue,
    pub unit: InkUnit,
    pub precision: ParamPrecision,
}

impl InkParameter {
    pub const ALL: [InkParameter; 4] = [Self::InkAngle, Self::InkFrequency, Self::NeutralDensity, Self::Solidity];
    /// The parameter's table token.
    pub const fn code(self) -> &'static str {
        match self {
            Self::InkAngle => "ink_angle",
            Self::InkFrequency => "ink_frequency",
            Self::NeutralDensity => "neutral_density",
            Self::Solidity => "solidity",
        }
    }
    /// The member with exactly this token; anything else is `None`.
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.code() == code)
    }
    /// The parameter's row of the STU-LAY-160 table, exactly as stated (unknown stays unknown).
    pub const fn spec(self) -> InkParameterSpec {
        use ParamValue::{Known, Unknown};
        let (hard_min, hard_max, unit, precision) = match self {
            Self::InkAngle => (0.0, 360.0, InkUnit::Deg, ParamPrecision::Unknown),
            Self::InkFrequency => (1.0, 500.0, InkUnit::Count, ParamPrecision::Unknown),
            Self::NeutralDensity => (0.001, 10.0, InkUnit::Ratio, ParamPrecision::Digits(3)),
            Self::Solidity => (0.0, 1.0, InkUnit::Ratio, ParamPrecision::Unknown),
        };
        InkParameterSpec {
            parameter: self,
            hard_min: Known(hard_min),
            hard_max: Known(hard_max),
            soft_min: Unknown,
            soft_max: Unknown,
            default: Unknown,
            unit,
            precision,
        }
    }
    /// Admits `value` for ink `ink`: finite, inside the hard bounds (inclusive) and, where the table
    /// declares a precision, with no more decimal places than it (tolerance for the binary form of a
    /// decimal only). Soft bounds and defaults are unknown and never applied; nothing is clamped or
    /// rounded.
    pub fn check(self, ink: &str, value: f64) -> Result<f64, InkError> {
        let spec = self.spec();
        let below = matches!(spec.hard_min, ParamValue::Known(lo) if value < lo);
        let above = matches!(spec.hard_max, ParamValue::Known(hi) if value > hi);
        if !value.is_finite() || below || above {
            return Err(self.out_of_range(ink));
        }
        if let ParamPrecision::Digits(places) = spec.precision {
            let scaled = value * 10f64.powi(i32::from(places));
            let tolerance = 1e-9 * scaled.abs().max(1.0);
            if (scaled - scaled.round()).abs() > tolerance {
                return Err(InkError::ParameterPrecision(ink.into(), self));
            }
        }
        Ok(value)
    }
    fn out_of_range(self, ink: &str) -> InkError {
        match self {
            Self::InkAngle => InkError::AngleOutOfRange(ink.into()),
            Self::InkFrequency => InkError::FrequencyOutOfRange(ink.into()),
            Self::NeutralDensity => InkError::NeutralDensityOutOfRange(ink.into()),
            Self::Solidity => InkError::SolidityOutOfRange(ink.into()),
        }
    }
}

/// One ink of the ink manager.
#[derive(Clone, Debug, PartialEq)]
pub struct Ink {
    /// Unique, non-empty; process inks use [`ProcessInk::name`].
    pub name: String,
    /// `None` = a spot ink.
    pub process: Option<ProcessInk>,
    pub ink_type: InkType,
    /// `ink_angle`, degrees, 0..=360.
    pub angle_deg: Option<f64>,
    /// `ink_frequency`, 1..=500.
    pub frequency: Option<f64>,
    /// `neutral_density`, 0.001..=10.
    pub neutral_density: Option<f64>,
    /// `solidity`, 0..=1.
    pub solidity: Option<f64>,
    /// Spot only: the ink this one prints as.
    pub alias: Option<String>,
    /// Spot only: separate this ink into process inks through `process_alternate`.
    pub convert_to_process: bool,
    /// Spot only: the CMYK (0..=1) of the ink at full tint, from its swatch alternate.
    pub process_alternate: Option<[f64; 4]>,
}
impl Ink {
    /// A process ink with no stated parameters.
    pub fn process(p: ProcessInk) -> Self {
        Self::spot(p.name(), None).with_process(p)
    }
    /// A spot ink with an optional CMYK alternate.
    pub fn spot(name: &str, process_alternate: Option<[f64; 4]>) -> Self {
        Self {
            name: name.to_owned(),
            process: None,
            ink_type: InkType::Normal,
            angle_deg: None,
            frequency: None,
            neutral_density: None,
            solidity: None,
            alias: None,
            convert_to_process: false,
            process_alternate,
        }
    }
    fn with_process(mut self, p: ProcessInk) -> Self {
        self.process = Some(p);
        self
    }
    /// The stored value of one tabled parameter (`None` = not stated; no default is supplied).
    pub const fn parameter(&self, p: InkParameter) -> Option<f64> {
        match p {
            InkParameter::InkAngle => self.angle_deg,
            InkParameter::InkFrequency => self.frequency,
            InkParameter::NeutralDensity => self.neutral_density,
            InkParameter::Solidity => self.solidity,
        }
    }
    fn parameter_slot(&mut self, p: InkParameter) -> &mut Option<f64> {
        match p {
            InkParameter::InkAngle => &mut self.angle_deg,
            InkParameter::InkFrequency => &mut self.frequency,
            InkParameter::NeutralDensity => &mut self.neutral_density,
            InkParameter::Solidity => &mut self.solidity,
        }
    }
}

/// One mutating edit of an [`InkManager`] (STU-LAY-160 ink manager controls).
#[derive(Clone, Debug, PartialEq)]
pub enum InkEdit {
    /// Add a spot ink with an optional CMYK alternate (0..=1).
    AddSpot { name: String, process_alternate: Option<[f64; 4]> },
    /// Set (`Some`) or clear (`None`) one tabled parameter of an ink.
    SetParameter { ink: String, parameter: InkParameter, value: Option<f64> },
    SetInkType { ink: String, ink_type: InkType },
    /// Alias a spot ink to another ink (`None` removes the alias); a process ink cannot be aliased.
    SetAlias { ink: String, alias: Option<String> },
    /// Per-ink spot-to-process conversion; a process ink has nothing to convert.
    SetConvertToProcess { ink: String, convert: bool },
    SetAllSpotsToProcess(bool),
    SetUseStandardLabForSpots(bool),
}
impl InkEdit {
    /// Stable code of the edit kind.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::AddSpot { .. } => "ink_edit.add_spot",
            Self::SetParameter { .. } => "ink_edit.set_parameter",
            Self::SetInkType { .. } => "ink_edit.set_ink_type",
            Self::SetAlias { .. } => "ink_edit.set_alias",
            Self::SetConvertToProcess { .. } => "ink_edit.set_convert_to_process",
            Self::SetAllSpotsToProcess(_) => "ink_edit.set_all_spots_to_process",
            Self::SetUseStandardLabForSpots(_) => "ink_edit.set_use_standard_lab_for_spots",
        }
    }
    fn ink(&self) -> Option<&str> {
        match self {
            Self::AddSpot { name: ink, .. }
            | Self::SetParameter { ink, .. }
            | Self::SetInkType { ink, .. }
            | Self::SetAlias { ink, .. }
            | Self::SetConvertToProcess { ink, .. } => Some(ink),
            Self::SetAllSpotsToProcess(_) | Self::SetUseStandardLabForSpots(_) => None,
        }
    }
}

/// Attribution of one applied ink edit: the caller's actor (transport identifiers only; it grants
/// nothing), the edit kind and the ink it touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InkEditReceipt {
    pub actor: ActorContext,
    pub edit: &'static str,
    pub ink: Option<String>,
}

/// The edited manager and its receipt; the input manager is unchanged.
#[derive(Clone, Debug, PartialEq)]
pub struct InkEditOutcome {
    pub manager: InkManager,
    pub receipt: InkEditReceipt,
}

/// The ink manager of a document or an output.
#[derive(Clone, Debug, PartialEq)]
pub struct InkManager {
    pub inks: Vec<Ink>,
    /// All spots to process.
    pub all_spots_to_process: bool,
    /// Use standard Lab values for spots (stored; the preview has no Lab path, see separations).
    pub use_standard_lab_for_spots: bool,
}
impl Default for InkManager {
    /// The four process inks, nothing converted.
    fn default() -> Self {
        Self {
            inks: ProcessInk::ALL.iter().map(|p| Ink::process(*p)).collect(),
            all_spots_to_process: false,
            use_standard_lab_for_spots: false,
        }
    }
}

/// A mixed ink: process and spot inks at fixed tints (0..=1).
#[derive(Clone, Debug, PartialEq)]
pub struct MixedInk {
    pub name: String,
    pub parts: Vec<(String, f64)>,
}

/// One row of a mixed-ink group: an ink, its initial tint, how many more steps and the step (0..=1).
#[derive(Clone, Debug, PartialEq)]
pub struct MixedInkGroupRow {
    pub ink: String,
    pub initial: f64,
    pub repeat: u32,
    pub increment: f64,
}

/// A mixed-ink group (generates `prod(repeat + 1)` mixed inks).
#[derive(Clone, Debug, PartialEq)]
pub struct MixedInkGroup {
    pub name: String,
    pub rows: Vec<MixedInkGroupRow>,
}

/// Most spot inks in one mixed ink (STU-LAY-160).
pub const MAX_MIXED_SPOT_INKS: usize = 27;
/// Most inks in one mixed ink (STU-LAY-160).
pub const MAX_MIXED_INKS: usize = 31;
/// Most swatches one mixed-ink group generates (STU-LAY-160).
pub const MAX_MIXED_GROUP_SWATCHES: u64 = 1000;
/// Most inks an ink manager lists.
pub const MAX_INKS: usize = 1024;

/// Typed ink-model refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InkError {
    InvalidName,
    DuplicateInk(String),
    UnknownInk(String),
    ProcessInkAliased(String),
    AliasCycle(String),
    AngleOutOfRange(String),
    FrequencyOutOfRange(String),
    NeutralDensityOutOfRange(String),
    SolidityOutOfRange(String),
    /// A value with more decimal places than its declared precision (`neutral_density`: 3).
    ParameterPrecision(String, InkParameter),
    /// Spot-to-process conversion requested on a process ink (Studio rule; UNVERIFIED, D5).
    ProcessInkNotConvertible(String),
    AlternateOutOfRange(String),
    TintOutOfRange(String),
    MixedInkEmpty(String),
    /// A mixed ink lists the same ink twice (its counts against the 27 / 31 limits would be ambiguous).
    MixedInkDuplicatePart(String),
    MixedInkTooManySpots(String),
    MixedInkTooManyInks(String),
    MixedInkGroupTooLarge(String),
    TooManyInks,
}
impl InkError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidName => "invalid_ink_name",
            Self::DuplicateInk(_) => "duplicate_ink",
            Self::UnknownInk(_) => "unknown_ink",
            Self::ProcessInkAliased(_) => "process_ink_cannot_be_aliased",
            Self::AliasCycle(_) => "ink_alias_cycle",
            Self::AngleOutOfRange(_) => "ink_angle_out_of_range",
            Self::FrequencyOutOfRange(_) => "ink_frequency_out_of_range",
            Self::NeutralDensityOutOfRange(_) => "neutral_density_out_of_range",
            Self::SolidityOutOfRange(_) => "solidity_out_of_range",
            Self::ParameterPrecision(..) => "ink_parameter_precision",
            Self::ProcessInkNotConvertible(_) => "process_ink_not_convertible",
            Self::AlternateOutOfRange(_) => "process_alternate_out_of_range",
            Self::TintOutOfRange(_) => "tint_out_of_range",
            Self::MixedInkEmpty(_) => "mixed_ink_empty",
            Self::MixedInkDuplicatePart(_) => "mixed_ink_duplicate_part",
            Self::MixedInkTooManySpots(_) => "mixed_ink_too_many_spot_inks",
            Self::MixedInkTooManyInks(_) => "mixed_ink_too_many_inks",
            Self::MixedInkGroupTooLarge(_) => "mixed_ink_group_too_large",
            Self::TooManyInks => "too_many_inks",
        }
    }

    /// The closed observe diagnostic code of this refusal (no text): the caps are `BudgetExceeded`.
    pub(crate) const fn diagnostic_code(&self) -> DiagnosticCode {
        match self {
            Self::InvalidName => DiagnosticCode::InvalidInput,
            Self::UnknownInk(_) => DiagnosticCode::InvalidTarget,
            Self::DuplicateInk(_) => DiagnosticCode::DuplicateAddress,
            Self::TooManyInks | Self::MixedInkTooManySpots(_) | Self::MixedInkTooManyInks(_) | Self::MixedInkGroupTooLarge(_) => {
                DiagnosticCode::BudgetExceeded
            }
            Self::ProcessInkAliased(_)
            | Self::AliasCycle(_)
            | Self::AngleOutOfRange(_)
            | Self::FrequencyOutOfRange(_)
            | Self::NeutralDensityOutOfRange(_)
            | Self::SolidityOutOfRange(_)
            | Self::ParameterPrecision(..)
            | Self::ProcessInkNotConvertible(_)
            | Self::AlternateOutOfRange(_)
            | Self::TintOutOfRange(_)
            | Self::MixedInkEmpty(_)
            | Self::MixedInkDuplicatePart(_) => DiagnosticCode::ValidationRejected,
        }
    }
}

fn tint_ok(t: f64) -> bool {
    t.is_finite() && (0.0..=1.0).contains(&t)
}

/// Where a spot ink's content prints.
#[derive(Clone, Debug, PartialEq)]
pub enum InkTarget {
    /// On the plate of this ink (an ink of the manager, after aliasing).
    Plate(String),
    /// Converted to process: CMYK at full tint.
    Process([f64; 4]),
    /// Converted to process but without a CMYK alternate: printed on its own plate (noted).
    PlateNoAlternate(String),
}

impl InkManager {
    /// The ink with this name.
    pub fn ink(&self, name: &str) -> Option<&Ink> {
        self.inks.iter().find(|i| i.name == name)
    }

    /// The declared `ink_type` of the ink with this name (`None` for an ink the manager does not list).
    pub fn ink_type(&self, name: &str) -> Option<InkType> {
        self.ink(name).map(|i| i.ink_type)
    }

    /// True when content in spot ink `name` reaches its plate through an alias (STU-LAY-160 ink aliasing).
    pub fn is_aliased(&self, name: &str) -> bool {
        self.ink(name).is_some_and(|i| i.alias.is_some())
    }

    /// Checks names, ranges, aliases (a process ink with an alias is refused, targets exist, no cycles) and
    /// process alternates.
    pub fn validate(&self) -> Result<(), InkError> {
        if self.inks.len() > MAX_INKS {
            return Err(InkError::TooManyInks);
        }
        let mut seen = std::collections::BTreeSet::new();
        for i in &self.inks {
            let n = i.name.as_str();
            if n.trim().is_empty() || n.len() > 256 {
                return Err(InkError::InvalidName);
            }
            if !seen.insert(n) {
                return Err(InkError::DuplicateInk(n.into()));
            }
            for p in InkParameter::ALL {
                if let Some(v) = i.parameter(p) {
                    p.check(n, v)?;
                }
            }
            if i.process.is_some() && i.alias.is_some() {
                return Err(InkError::ProcessInkAliased(n.into()));
            }
            if i.process_alternate.is_some_and(|c| !c.iter().all(|v| tint_ok(*v))) {
                return Err(InkError::AlternateOutOfRange(n.into()));
            }
        }
        for i in &self.inks {
            self.target(&i.name)?;
        }
        Ok(())
    }

    /// Where content in spot ink `name` prints: the alias chain is followed (bounded by the ink
    /// count; a loop is `ink_alias_cycle`), then the conversion settings of the ink reached. An ink
    /// the manager does not list prints on its own plate (a separation lists every ink it meets).
    pub fn target(&self, name: &str) -> Result<InkTarget, InkError> {
        let mut at = name;
        for _ in 0..=self.inks.len() {
            let Some(ink) = self.ink(at) else {
                return if at == name { Ok(InkTarget::Plate(name.into())) } else { Err(InkError::UnknownInk(at.into())) };
            };
            if let Some(next) = &ink.alias {
                at = next.as_str();
                continue;
            }
            if ink.process.is_none() && (ink.convert_to_process || self.all_spots_to_process) {
                return Ok(match ink.process_alternate {
                    Some(c) => InkTarget::Process(c),
                    None => InkTarget::PlateNoAlternate(ink.name.clone()),
                });
            }
            return Ok(InkTarget::Plate(ink.name.clone()));
        }
        Err(InkError::AliasCycle(name.into()))
    }
}

impl InkManager {
    /// Applies one edit for `actor` (STU-LAY-160 ink manager; HOST rule 4: the caller's actor, never
    /// a default). The edit is made on a copy, the whole copy is validated ([`InkManager::validate`]:
    /// names, parameter bounds and precision, aliases), and only a valid copy is returned with its
    /// receipt; on any refusal `self` is unchanged and nothing partial exists. Typed refusals:
    /// `unknown_ink`, `duplicate_ink`, `invalid_ink_name`, `too_many_inks`, the parameter range and
    /// precision codes, `process_ink_cannot_be_aliased`, `ink_alias_cycle`,
    /// `process_ink_not_convertible`, `process_alternate_out_of_range`.
    pub fn apply_edit(&self, actor: &ActorContext, edit: &InkEdit) -> Result<InkEditOutcome, InkError> {
        let mut next = self.clone();
        match edit {
            InkEdit::AddSpot { name, process_alternate } => {
                if next.inks.len() >= MAX_INKS {
                    return Err(InkError::TooManyInks);
                }
                if name.trim().is_empty() || name.len() > 256 {
                    return Err(InkError::InvalidName);
                }
                if next.ink(name).is_some() {
                    return Err(InkError::DuplicateInk(name.clone()));
                }
                next.inks.push(Ink::spot(name, *process_alternate));
            }
            InkEdit::SetParameter { ink, parameter, value } => {
                let checked = value.map(|v| parameter.check(ink, v)).transpose()?;
                *next.ink_mut(ink)?.parameter_slot(*parameter) = checked;
            }
            InkEdit::SetInkType { ink, ink_type } => next.ink_mut(ink)?.ink_type = *ink_type,
            InkEdit::SetAlias { ink, alias } => {
                let target = next.ink_mut(ink)?;
                if target.process.is_some() && alias.is_some() {
                    return Err(InkError::ProcessInkAliased(ink.clone()));
                }
                target.alias = alias.clone();
                if let Some(a) = alias
                    && self.ink(a).is_none()
                {
                    return Err(InkError::UnknownInk(a.clone()));
                }
            }
            InkEdit::SetConvertToProcess { ink, convert } => {
                let target = next.ink_mut(ink)?;
                if target.process.is_some() && *convert {
                    return Err(InkError::ProcessInkNotConvertible(ink.clone()));
                }
                target.convert_to_process = *convert;
            }
            InkEdit::SetAllSpotsToProcess(on) => next.all_spots_to_process = *on,
            InkEdit::SetUseStandardLabForSpots(on) => next.use_standard_lab_for_spots = *on,
        }
        next.validate()?;
        let receipt = InkEditReceipt { actor: actor.clone(), edit: edit.code(), ink: edit.ink().map(str::to_owned) };
        Ok(InkEditOutcome { manager: next, receipt })
    }

    /// [`InkManager::apply_edit`] plus exactly one terminal emission through observe (HOST INTEGRATION
    /// rule 5). The returned result is the edit's own, whatever the emission did.
    ///
    /// Terminal codes (the typed [`Emission::code`]): `ink_edit.applied` is a success with disposition
    /// changed and counters `[0, 1, 0, 1, 1]` (one write attempted, admitted and changed);
    /// `ink_edit.no_change` is a success whose edited manager equals the input, disposition no_change,
    /// `[0, 1, 0, 1, 0]`. A refusal carries its [`InkError::code`] as a rejected validation failure with
    /// `[0, 1, 0, 0, 0]` and the diagnostic code `InvalidInput` (`invalid_ink_name`), `InvalidTarget`
    /// (`unknown_ink`), `DuplicateAddress` (`duplicate_ink`), `BudgetExceeded` (the ink, mixed-ink and
    /// swatch caps) or `ValidationRejected` (every other refusal). No ink name or value is emitted.
    pub fn apply_edit_observed(
        &self,
        actor: &ActorContext,
        edit: &InkEdit,
        route: ObserveRoute<'_>,
    ) -> (Result<InkEditOutcome, InkError>, Emission) {
        let result = self.apply_edit(actor, edit);
        let emission = match &result {
            Ok(outcome) if outcome.manager == *self => {
                route.terminal("ink_edit.no_change", Outcome::Success, Disposition::NoChange, None, [0, 1, 0, 1, 0])
            }
            Ok(_) => route.terminal("ink_edit.applied", Outcome::Success, Disposition::Changed, None, [0, 1, 0, 1, 1]),
            Err(e) => route.terminal(
                e.code(),
                Outcome::Failure(FailureCode::Validation),
                Disposition::Rejected,
                Some(e.diagnostic_code()),
                [0, 1, 0, 0, 0],
            ),
        };
        (result, emission)
    }

    fn ink_mut(&mut self, name: &str) -> Result<&mut Ink, InkError> {
        self.inks.iter_mut().find(|i| i.name == name).ok_or_else(|| InkError::UnknownInk(name.into()))
    }
}

impl MixedInk {
    /// Captured mixed-ink limits and references: parts exist in `inks`, tints 0..=1, at most 27
    /// spot and 31 total parts, at least one part.
    pub fn validate(&self, inks: &InkManager) -> Result<(), InkError> {
        let n = self.name.as_str();
        if n.trim().is_empty() {
            return Err(InkError::InvalidName);
        }
        if self.parts.is_empty() {
            return Err(InkError::MixedInkEmpty(n.into()));
        }
        if self.parts.len() > MAX_MIXED_INKS {
            return Err(InkError::MixedInkTooManyInks(n.into()));
        }
        let mut spots = 0usize;
        let mut seen = std::collections::BTreeSet::new();
        for (ink, t) in &self.parts {
            if !seen.insert(ink.as_str()) {
                return Err(InkError::MixedInkDuplicatePart(n.into()));
            }
            let def = inks.ink(ink).ok_or_else(|| InkError::UnknownInk(ink.clone()))?;
            if def.process.is_none() {
                spots = spots.saturating_add(1);
            }
            if !tint_ok(*t) {
                return Err(InkError::TintOutOfRange(n.into()));
            }
        }
        if spots > MAX_MIXED_SPOT_INKS {
            return Err(InkError::MixedInkTooManySpots(n.into()));
        }
        Ok(())
    }
}

impl MixedInkGroup {
    /// Number of swatches the group generates (`prod(repeat + 1)`), or `mixed_ink_group_too_large`
    /// past 1000 (checked arithmetic on hostile repeat counts; test ink_manager_aliases_conversion_and_mixed_inks).
    pub fn swatch_count(&self) -> Result<u64, InkError> {
        let mut n: u64 = 1;
        for r in &self.rows {
            n = n
                .checked_mul(u64::from(r.repeat).saturating_add(1))
                .filter(|v| *v <= MAX_MIXED_GROUP_SWATCHES)
                .ok_or_else(|| InkError::MixedInkGroupTooLarge(self.name.clone()))?;
        }
        Ok(n)
    }

    /// The group's mixed inks, named `"<group> <k>"` (k from 1), every combination of the rows' tint
    /// steps, the first row varying slowest. Each generated mixed ink is validated against `inks`.
    pub fn generate(&self, inks: &InkManager) -> Result<Vec<MixedInk>, InkError> {
        let count = self.swatch_count()?;
        if self.rows.is_empty() {
            return Err(InkError::MixedInkEmpty(self.name.clone()));
        }
        for r in &self.rows {
            let last = r.initial + f64::from(r.repeat) * r.increment;
            if !tint_ok(r.initial) || !r.increment.is_finite() || !tint_ok(last) {
                return Err(InkError::TintOutOfRange(self.name.clone()));
            }
        }
        let count = usize::try_from(count).map_err(|_| InkError::MixedInkGroupTooLarge(self.name.clone()))?;
        let mut out = Vec::with_capacity(count);
        let mut steps = vec![0u32; self.rows.len()];
        for k in 0..count {
            let parts = self
                .rows
                .iter()
                .zip(&steps)
                .map(|(r, s)| (r.ink.clone(), r.initial + f64::from(*s) * r.increment))
                .collect();
            let m = MixedInk { name: format!("{} {}", self.name, k.saturating_add(1)), parts };
            m.validate(inks)?;
            out.push(m);
            // Odometer: the last row varies fastest.
            for (s, r) in steps.iter_mut().zip(&self.rows).rev() {
                if *s < r.repeat {
                    *s = s.saturating_add(1);
                    break;
                }
                *s = 0;
            }
        }
        Ok(out)
    }
}
