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
#![deny(clippy::arithmetic_side_effects)]

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
    pub const fn code(self) -> &'static str {
        match self {
            Self::Normal => "ink.normal",
            Self::Opaque => "ink.opaque",
            Self::Transparent => "ink.transparent",
            Self::OpaqueIgnore => "ink.opaque_ignore",
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
    AlternateOutOfRange(String),
    TintOutOfRange(String),
    MixedInkEmpty(String),
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
            Self::AlternateOutOfRange(_) => "process_alternate_out_of_range",
            Self::TintOutOfRange(_) => "tint_out_of_range",
            Self::MixedInkEmpty(_) => "mixed_ink_empty",
            Self::MixedInkTooManySpots(_) => "mixed_ink_too_many_spot_inks",
            Self::MixedInkTooManyInks(_) => "mixed_ink_too_many_inks",
            Self::MixedInkGroupTooLarge(_) => "mixed_ink_group_too_large",
            Self::TooManyInks => "too_many_inks",
        }
    }
}

fn in_range(v: Option<f64>, lo: f64, hi: f64) -> bool {
    v.is_none_or(|x| x.is_finite() && (lo..=hi).contains(&x))
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
            if !in_range(i.angle_deg, 0.0, 360.0) {
                return Err(InkError::AngleOutOfRange(n.into()));
            }
            if !in_range(i.frequency, 1.0, 500.0) {
                return Err(InkError::FrequencyOutOfRange(n.into()));
            }
            if !in_range(i.neutral_density, 0.001, 10.0) {
                return Err(InkError::NeutralDensityOutOfRange(n.into()));
            }
            if !in_range(i.solidity, 0.0, 1.0) {
                return Err(InkError::SolidityOutOfRange(n.into()));
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
        for (ink, t) in &self.parts {
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
