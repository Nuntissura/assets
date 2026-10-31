//! Export-stage settings entries (lane SMB-SETTINGS-SCHEMA-1, RF-09 phase 0): [`OutputSizing`],
//! [`OutputSharpening`] and the scalar fields of [`WatermarkSettings`] as typed accord
//! [`SettingEntry`] values, module tab `develop`, feature subtab `export`. Additive: the stage and
//! its types are unchanged.
//!
//! - Keys are the [`crate::export_stage::EXPORT_STAGE_KEYS`] names in kebab case. The watermark
//!   graphic / text source gets no entry (a caller-supplied image, not a preference). The one
//!   `watermark_size` key is two entries: the rule (`proportional`, `fit`, `fill`) and the
//!   proportional percent (used by `proportional` only), a Studio reading of the split.
//! - Enum members reuse the existing `spec_member` strings (resize type, size unit, resolution
//!   unit, sharpening media); the anchor members are the snake-case names of the stage key default
//!   (`bottom_right`); the rotation members are the clockwise degrees.
//! - Bounds: the lower bounds are STU-IO-137's hard minimum 1 (max width / height, percentage,
//!   resolution) and the stage's own domains (percent 0..=100, proportional size above 0, level
//!   1..=3, megapixels above 0). STU-IO-137 names no hard maximum (STU-IO-103: not clamped by the
//!   stage), so every upper bound is `StudioDeclared` and lies inside what
//!   [`crate::export_stage::validate_export_stage`] admits for every combination of the other
//!   entries: the resolution maximum 4000 keeps the widest sharpening kernel (Matte at 4000 ppcm =
//!   10160 ppi, sigma 63.5 px) under [`crate::export_stage::MAX_SHARPEN_SIGMA_PX`]
//!   (`tests/settings_entries.rs`). Lightroom's ranges are UNVERIFIED (D5).
//! - `bound_source` is `Spec` when the spec fixes the whole domain (a STU-IO-136 member list, the
//!   STU-IO-137 level 1..=3, a STU-IO-138 named toggle) and `StudioDeclared` otherwise.
use crate::export_stage::{OutputSharpening, OutputSizing, WatermarkAnchor, WatermarkRotation, WatermarkSettings, WatermarkSize};
use hsk_studio_accord::{BoundSource, Redaction, SettingConstraint, SettingEntry, SettingScope, SettingValue};

/// Module tab of these entries.
pub const MODULE: &str = "develop";
/// Feature subtab of these entries.
pub const FEATURE: &str = "export";

pub const RESIZE_TYPE_MEMBERS: &[&str] = &["WidthHeight", "Dimensions", "LongEdge", "ShortEdge", "Megapixels", "Percentage"];
pub const SIZE_UNIT_MEMBERS: &[&str] = &["pixels", "in", "cm"];
pub const RESOLUTION_UNIT_MEMBERS: &[&str] = &["ppinch", "ppcm"];
pub const SHARPEN_MEDIA_MEMBERS: &[&str] = &["Screen", "Glossy", "Matte"];
pub const WATERMARK_SIZE_MEMBERS: &[&str] = &["proportional", "fit", "fill"];
pub const WATERMARK_ANCHOR_MEMBERS: &[&str] =
    &["top_left", "top", "top_right", "left", "centre", "right", "bottom_left", "bottom", "bottom_right"];
pub const WATERMARK_ROTATION_MEMBERS: &[&str] = &["0", "90", "180", "270"];

/// The enum member of an anchor.
pub const fn anchor_member(a: WatermarkAnchor) -> &'static str {
    match a {
        WatermarkAnchor::TopLeft => "top_left",
        WatermarkAnchor::Top => "top",
        WatermarkAnchor::TopRight => "top_right",
        WatermarkAnchor::Left => "left",
        WatermarkAnchor::Centre => "centre",
        WatermarkAnchor::Right => "right",
        WatermarkAnchor::BottomLeft => "bottom_left",
        WatermarkAnchor::Bottom => "bottom",
        WatermarkAnchor::BottomRight => "bottom_right",
    }
}

/// The enum member of a rotation (clockwise degrees).
pub const fn rotation_member(r: WatermarkRotation) -> &'static str {
    match r {
        WatermarkRotation::R0 => "0",
        WatermarkRotation::R90 => "90",
        WatermarkRotation::R180 => "180",
        WatermarkRotation::R270 => "270",
    }
}

pub const CONSTRAIN_SIZE: &str = "studio.develop.export.constrain-size";
pub const RESIZE_TYPE: &str = "studio.develop.export.resize-type";
pub const MAX_WIDTH: &str = "studio.develop.export.max-width";
pub const MAX_HEIGHT: &str = "studio.develop.export.max-height";
pub const SIZE_UNIT: &str = "studio.develop.export.size-unit";
pub const RESIZE_PERCENTAGE: &str = "studio.develop.export.resize-percentage";
pub const MEGAPIXELS: &str = "studio.develop.export.megapixels";
pub const RESOLUTION: &str = "studio.develop.export.resolution";
pub const RESOLUTION_UNIT: &str = "studio.develop.export.resolution-unit";
pub const DO_NOT_ENLARGE: &str = "studio.develop.export.do-not-enlarge";
pub const SHARPENING_ENABLED: &str = "studio.develop.export.output-sharpening-enabled";
pub const SHARPENING_MEDIA: &str = "studio.develop.export.output-sharpening-media";
pub const SHARPENING_LEVEL: &str = "studio.develop.export.output-sharpening-level";
pub const USE_WATERMARK: &str = "studio.develop.export.use-watermark";
pub const WATERMARK_OPACITY: &str = "studio.develop.export.watermark-opacity";
pub const WATERMARK_SIZE: &str = "studio.develop.export.watermark-size";
pub const WATERMARK_SIZE_PERCENT: &str = "studio.develop.export.watermark-size-percent";
pub const WATERMARK_ANCHOR: &str = "studio.develop.export.watermark-anchor";
pub const WATERMARK_INSET_X: &str = "studio.develop.export.watermark-inset-x";
pub const WATERMARK_INSET_Y: &str = "studio.develop.export.watermark-inset-y";
pub const WATERMARK_ROTATION: &str = "studio.develop.export.watermark-rotation";

const fn entry(
    id: &'static str,
    label: &'static str,
    unit: Option<&'static str>,
    constraint: SettingConstraint,
    default: SettingValue<'static>,
    bound_source: BoundSource,
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
        bound_source,
    }
}

const SPEC: BoundSource = BoundSource::Spec;
const STUDIO: BoundSource = BoundSource::StudioDeclared;
const fn float(min: f64, max: f64) -> SettingConstraint {
    SettingConstraint::FloatRange { min, max }
}

/// The export-stage entries in stage order (image sizing, output sharpening, watermarking).
pub const EXPORT_SETTING_ENTRIES: &[SettingEntry] = &[
    entry(CONSTRAIN_SIZE, "Resize to fit", None, SettingConstraint::Bool, SettingValue::Bool(true), SPEC),
    entry(RESIZE_TYPE, "Resize type", None, SettingConstraint::Enum(RESIZE_TYPE_MEMBERS), SettingValue::Str("WidthHeight"), SPEC),
    entry(MAX_WIDTH, "Width", None, float(1.0, 65_535.0), SettingValue::Float(1000.0), STUDIO),
    entry(MAX_HEIGHT, "Height", None, float(1.0, 65_535.0), SettingValue::Float(1000.0), STUDIO),
    entry(SIZE_UNIT, "Size unit", None, SettingConstraint::Enum(SIZE_UNIT_MEMBERS), SettingValue::Str("pixels"), SPEC),
    entry(RESIZE_PERCENTAGE, "Resize percentage", Some("%"), float(1.0, 1000.0), SettingValue::Float(100.0), STUDIO),
    entry(MEGAPIXELS, "Megapixels", Some("MP"), float(0.01, 268.0), SettingValue::Float(2.0), STUDIO),
    entry(RESOLUTION, "Resolution", None, float(1.0, 4000.0), SettingValue::Float(240.0), STUDIO),
    entry(RESOLUTION_UNIT, "Resolution unit", None, SettingConstraint::Enum(RESOLUTION_UNIT_MEMBERS), SettingValue::Str("ppinch"), SPEC),
    entry(DO_NOT_ENLARGE, "Don't enlarge", None, SettingConstraint::Bool, SettingValue::Bool(false), SPEC),
    entry(SHARPENING_ENABLED, "Sharpen for output", None, SettingConstraint::Bool, SettingValue::Bool(false), STUDIO),
    entry(SHARPENING_MEDIA, "Sharpen for", None, SettingConstraint::Enum(SHARPEN_MEDIA_MEMBERS), SettingValue::Str("Screen"), SPEC),
    entry(SHARPENING_LEVEL, "Sharpening amount", None, SettingConstraint::IntRange { min: 1, max: 3 }, SettingValue::Int(2), SPEC),
    entry(USE_WATERMARK, "Watermark", None, SettingConstraint::Bool, SettingValue::Bool(false), SPEC),
    entry(WATERMARK_OPACITY, "Watermark opacity", Some("%"), float(0.0, 100.0), SettingValue::Float(100.0), STUDIO),
    entry(WATERMARK_SIZE, "Watermark size", None, SettingConstraint::Enum(WATERMARK_SIZE_MEMBERS), SettingValue::Str("proportional"), STUDIO),
    entry(WATERMARK_SIZE_PERCENT, "Watermark proportional size", Some("%"), float(0.1, 100.0), SettingValue::Float(25.0), STUDIO),
    entry(WATERMARK_ANCHOR, "Watermark anchor", None, SettingConstraint::Enum(WATERMARK_ANCHOR_MEMBERS), SettingValue::Str("bottom_right"), STUDIO),
    entry(WATERMARK_INSET_X, "Watermark horizontal inset", Some("%"), float(0.0, 100.0), SettingValue::Float(0.0), STUDIO),
    entry(WATERMARK_INSET_Y, "Watermark vertical inset", Some("%"), float(0.0, 100.0), SettingValue::Float(0.0), STUDIO),
    entry(WATERMARK_ROTATION, "Watermark rotation", Some("deg"), SettingConstraint::Enum(WATERMARK_ROTATION_MEMBERS), SettingValue::Str("0"), STUDIO),
];

/// The current values of `s` as `(id, value)` pairs in [`EXPORT_SETTING_ENTRIES`] order (image sizing).
pub fn sizing_values(s: &OutputSizing) -> [(&'static str, SettingValue<'static>); 10] {
    [
        (CONSTRAIN_SIZE, SettingValue::Bool(s.constrain_size)),
        (RESIZE_TYPE, SettingValue::Str(s.resize_type.spec_member())),
        (MAX_WIDTH, SettingValue::Float(s.max_width)),
        (MAX_HEIGHT, SettingValue::Float(s.max_height)),
        (SIZE_UNIT, SettingValue::Str(s.size_unit.spec_member())),
        (RESIZE_PERCENTAGE, SettingValue::Float(s.percentage)),
        (MEGAPIXELS, SettingValue::Float(s.megapixels)),
        (RESOLUTION, SettingValue::Float(s.resolution)),
        (RESOLUTION_UNIT, SettingValue::Str(s.resolution_unit.spec_member())),
        (DO_NOT_ENLARGE, SettingValue::Bool(s.do_not_enlarge)),
    ]
}

/// The current values of `s` as `(id, value)` pairs (output sharpening).
pub fn sharpening_values(s: &OutputSharpening) -> [(&'static str, SettingValue<'static>); 3] {
    [
        (SHARPENING_ENABLED, SettingValue::Bool(s.enabled)),
        (SHARPENING_MEDIA, SettingValue::Str(s.media.spec_member())),
        (SHARPENING_LEVEL, SettingValue::Int(i64::from(s.level))),
    ]
}

/// The current scalar values of `s` as `(id, value)` pairs (watermarking; no source). The
/// proportional percent is reported only for a `Proportional` size.
pub fn watermark_values(s: &WatermarkSettings) -> Vec<(&'static str, SettingValue<'static>)> {
    let mut v = vec![
        (USE_WATERMARK, SettingValue::Bool(s.use_watermark)),
        (WATERMARK_OPACITY, SettingValue::Float(s.opacity_percent)),
        (WATERMARK_SIZE, SettingValue::Str(s.size.rule())),
    ];
    if let WatermarkSize::Proportional(p) = s.size {
        v.push((WATERMARK_SIZE_PERCENT, SettingValue::Float(p)));
    }
    v.extend([
        (WATERMARK_ANCHOR, SettingValue::Str(anchor_member(s.anchor))),
        (WATERMARK_INSET_X, SettingValue::Float(s.inset_x_percent)),
        (WATERMARK_INSET_Y, SettingValue::Float(s.inset_y_percent)),
        (WATERMARK_ROTATION, SettingValue::Str(rotation_member(s.rotation))),
    ]);
    v
}
