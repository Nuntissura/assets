//! Selection-tool settings entries (lane SMB-SETTINGS-SCHEMA-1, RF-09 phase 0): the magic wand
//! ([`WandOptions`]) and magnetic lasso ([`MagneticOptions`]) options as typed accord
//! [`SettingEntry`] values, module tab `pigment`, feature subtab `selection`. Additive: the option
//! types and the tools are unchanged.
//!
//! Every default equals the option type's `Default` field for field
//! (`tests/settings_entries.rs`). Every bound lies inside the engine domain, so a value the entry
//! admits is never refused by the tool: the wand admits any finite tolerance `>= 0` and any odd
//! sample size, the magnetic lasso any finite width and contrast `>= 0`, frequency `> 0` and any
//! margin. The upper bounds, the frequency floor and the sample-size members are Studio readings
//! (`StudioDeclared`; Photoshop's ranges and sample-size menu are UNVERIFIED, D5).
use crate::select_tools::magnetic::MagneticOptions;
use crate::select_tools::wand::{Connectivity, WandOptions};
use hsk_studio_accord::{BoundSource, Redaction, SettingConstraint, SettingEntry, SettingScope, SettingValue};

/// Module tab of these entries.
pub const MODULE: &str = "pigment";
/// Feature subtab of these entries.
pub const FEATURE: &str = "selection";

/// Wand sample-size members (odd square sides; `"1"` is the point sample).
pub const WAND_SAMPLE_SIZE_MEMBERS: &[&str] = &["1", "3", "5", "11", "31", "51", "101"];
/// Wand connectivity members.
pub const WAND_CONNECTIVITY_MEMBERS: &[&str] = &["four", "eight"];

/// The enum member of a connectivity.
pub const fn connectivity_member(c: Connectivity) -> &'static str {
    match c {
        Connectivity::Four => "four",
        Connectivity::Eight => "eight",
    }
}

const fn entry(
    id: &'static str,
    label: &'static str,
    unit: Option<&'static str>,
    constraint: SettingConstraint,
    default: SettingValue<'static>,
) -> SettingEntry {
    SettingEntry {
        id,
        module: MODULE,
        feature: FEATURE,
        label,
        unit,
        constraint,
        default,
        scope: SettingScope::Workspace,
        redaction: Redaction::Public,
        bound_source: BoundSource::StudioDeclared,
    }
}

pub const WAND_TOLERANCE: &str = "studio.pigment.selection.wand-tolerance";
pub const WAND_CONTIGUOUS: &str = "studio.pigment.selection.wand-contiguous";
pub const WAND_SAMPLE_MERGED: &str = "studio.pigment.selection.wand-sample-merged";
pub const WAND_SAMPLE_SIZE: &str = "studio.pigment.selection.wand-sample-size";
pub const WAND_CONNECTIVITY: &str = "studio.pigment.selection.wand-connectivity";
pub const MAGNETIC_WIDTH: &str = "studio.pigment.selection.magnetic-width";
pub const MAGNETIC_CONTRAST: &str = "studio.pigment.selection.magnetic-contrast";
pub const MAGNETIC_FREQUENCY: &str = "studio.pigment.selection.magnetic-frequency";
pub const MAGNETIC_SAMPLE_MERGED: &str = "studio.pigment.selection.magnetic-sample-merged";
pub const MAGNETIC_MARGIN: &str = "studio.pigment.selection.magnetic-margin";

/// The selection entries (wand, then magnetic lasso), in field order of their option types.
pub const SELECTION_SETTING_ENTRIES: &[SettingEntry] = &[
    entry(WAND_TOLERANCE, "Magic wand tolerance", Some("8-bit levels"), SettingConstraint::FloatRange { min: 0.0, max: 255.0 }, SettingValue::Float(32.0)),
    entry(WAND_CONTIGUOUS, "Magic wand contiguous", None, SettingConstraint::Bool, SettingValue::Bool(true)),
    entry(WAND_SAMPLE_MERGED, "Magic wand sample all layers", None, SettingConstraint::Bool, SettingValue::Bool(false)),
    entry(WAND_SAMPLE_SIZE, "Magic wand sample size", Some("px"), SettingConstraint::Enum(WAND_SAMPLE_SIZE_MEMBERS), SettingValue::Str("1")),
    entry(WAND_CONNECTIVITY, "Magic wand connectivity", None, SettingConstraint::Enum(WAND_CONNECTIVITY_MEMBERS), SettingValue::Str("four")),
    entry(MAGNETIC_WIDTH, "Magnetic lasso width", Some("px"), SettingConstraint::FloatRange { min: 0.0, max: 256.0 }, SettingValue::Float(10.0)),
    entry(MAGNETIC_CONTRAST, "Magnetic lasso contrast", Some("%"), SettingConstraint::FloatRange { min: 0.0, max: 100.0 }, SettingValue::Float(10.0)),
    entry(MAGNETIC_FREQUENCY, "Magnetic lasso frequency", None, SettingConstraint::FloatRange { min: 1.0, max: 100.0 }, SettingValue::Float(57.0)),
    entry(MAGNETIC_SAMPLE_MERGED, "Magnetic lasso sample all layers", None, SettingConstraint::Bool, SettingValue::Bool(false)),
    entry(MAGNETIC_MARGIN, "Magnetic lasso search margin", Some("px"), SettingConstraint::IntRange { min: 0, max: 1024 }, SettingValue::Int(16)),
];

/// The current values of `o` as `(id, value)` pairs, in [`SELECTION_SETTING_ENTRIES`] order for
/// the wand ids (a host writes these as PreferenceRecord values).
pub fn wand_values(o: &WandOptions) -> [(&'static str, SettingValue<'static>); 5] {
    let size = WAND_SAMPLE_SIZE_MEMBERS.iter().copied().find(|m| m.parse::<u32>().ok() == Some(o.sample_size)).unwrap_or("");
    [
        (WAND_TOLERANCE, SettingValue::Float(o.tolerance)),
        (WAND_CONTIGUOUS, SettingValue::Bool(o.contiguous)),
        (WAND_SAMPLE_MERGED, SettingValue::Bool(o.sample_merged)),
        // A sample size outside the members has no member: "" (refused as unknown_enum_member).
        (WAND_SAMPLE_SIZE, SettingValue::Str(size)),
        (WAND_CONNECTIVITY, SettingValue::Str(connectivity_member(o.connectivity))),
    ]
}

/// The current values of `o` as `(id, value)` pairs, in [`SELECTION_SETTING_ENTRIES`] order for
/// the magnetic lasso ids.
pub fn magnetic_values(o: &MagneticOptions) -> [(&'static str, SettingValue<'static>); 5] {
    [
        (MAGNETIC_WIDTH, SettingValue::Float(o.width)),
        (MAGNETIC_CONTRAST, SettingValue::Float(o.contrast)),
        (MAGNETIC_FREQUENCY, SettingValue::Float(o.frequency)),
        (MAGNETIC_SAMPLE_MERGED, SettingValue::Bool(o.sample_merged)),
        (MAGNETIC_MARGIN, SettingValue::Int(i64::from(o.margin_px))),
    ]
}
