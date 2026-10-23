//! Folio motion presets <-> motion presets (lane SMB-AE-PARTICLES-AUDIO-PRESETS slice 3; feature
//! `motion`; STU-MOT-110/111). The stored record (`hsk_studio_folio::StudioMotionPreset`, the
//! Studio-native style-registry entry) converts to and from `hsk_studio_motion::MotionPreset`, which
//! motion applies path-based onto a layer's property tree with a time offset
//! ([`apply_preset_record`]). Keyframes convert with the composition keyframe mapping (the same
//! interpolation and tangent mapping as composition streams). Conversion is total and lossless for
//! every field the record carries; record validation is Folio's (call
//! `validate_motion_presets` / `parse_motion_presets` first).
//!
//! An After Effects `.ffx` file is never read: [`import_preset_bytes`] refuses it with
//! [`PresetImportError::ProprietaryFfx`] (Folio's `MOTION_PRESET_FFX` refusal), and every other
//! input goes through Folio's wire parser.
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{
    Budget, Code, StudioCompositionKeyframe, StudioInterpolation, StudioKeyedValue, StudioMotionPreset,
    StudioMotionPresetEffect, StudioMotionPresetProperty, StudioMotionPresetSet, StudioTemporalTangent,
    MOTION_PRESET_FFX, parse_motion_presets,
};
use hsk_studio_motion::{
    AppliedPreset, Interpolation, Keyframe, MotionPreset, PresetEffect, PresetEntry, PresetEntryKind, PresetError,
    PropertyStore, TemporalTangent, apply_preset,
};
use std::fmt;

/// A refused preset import.
#[derive(Clone, Debug, PartialEq)]
pub enum PresetImportError {
    /// After Effects `.ffx` (proprietary binary): use a Studio-native preset.
    ProprietaryFfx,
    /// Folio refused the wire record (`Code:target`).
    Record(String),
}

impl PresetImportError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ProprietaryFfx => "preset_ffx_proprietary",
            Self::Record(_) => "preset_record",
        }
    }
}

impl fmt::Display for PresetImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Record(t) => write!(f, "{}:{t}", self.code()),
            Self::ProprietaryFfx => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for PresetImportError {}

/// Parses preset bytes: `.ffx` is [`PresetImportError::ProprietaryFfx`]; anything else must be the
/// Studio-native wire record.
pub fn import_preset_bytes(input: &[u8], budget: Budget, cancel: &CancellationToken) -> Result<StudioMotionPresetSet, PresetImportError> {
    parse_motion_presets(input, budget, cancel).map_err(|d| {
        if d.code == Code::UnsupportedSchema && d.target == MOTION_PRESET_FFX {
            PresetImportError::ProprietaryFfx
        } else {
            PresetImportError::Record(format!("{:?}:{}", d.code, d.target))
        }
    })
}

fn to_motion_interp(i: StudioInterpolation) -> Interpolation {
    match i {
        StudioInterpolation::Linear => Interpolation::Linear,
        StudioInterpolation::Bezier => Interpolation::Bezier,
        StudioInterpolation::ContinuousBezier => Interpolation::ContinuousBezier,
        StudioInterpolation::AutoBezier => Interpolation::AutoBezier,
        StudioInterpolation::Hold => Interpolation::Hold,
    }
}

fn to_folio_interp(i: Interpolation) -> StudioInterpolation {
    match i {
        Interpolation::Linear => StudioInterpolation::Linear,
        Interpolation::Bezier => StudioInterpolation::Bezier,
        Interpolation::ContinuousBezier => StudioInterpolation::ContinuousBezier,
        Interpolation::AutoBezier => StudioInterpolation::AutoBezier,
        Interpolation::Hold => StudioInterpolation::Hold,
    }
}

fn keyframes(v: &StudioKeyedValue) -> Vec<Keyframe> {
    let tangents = |t: &[StudioTemporalTangent]| -> Vec<TemporalTangent> {
        t.iter().map(|t| TemporalTangent { speed: t.speed, influence: t.influence }).collect()
    };
    v.keyframes
        .iter()
        .map(|k| Keyframe {
            time: Ticks::new(k.time),
            value: k.value.clone(),
            in_interp: to_motion_interp(k.in_interp),
            out_interp: to_motion_interp(k.out_interp),
            in_tangent: tangents(&k.in_tangent),
            out_tangent: tangents(&k.out_tangent),
        })
        .collect()
}

fn keyed(static_value: &[f64], keys: &[Keyframe]) -> StudioKeyedValue {
    let tangents = |t: &[TemporalTangent]| -> Vec<StudioTemporalTangent> {
        t.iter().map(|t| StudioTemporalTangent { speed: t.speed, influence: t.influence }).collect()
    };
    StudioKeyedValue {
        static_value: static_value.to_vec(),
        keyframes: keys
            .iter()
            .map(|k| StudioCompositionKeyframe {
                time: k.time.value(),
                value: k.value.clone(),
                in_interp: to_folio_interp(k.in_interp),
                out_interp: to_folio_interp(k.out_interp),
                in_tangent: tangents(&k.in_tangent),
                out_tangent: tangents(&k.out_tangent),
            })
            .collect(),
    }
}

/// The motion preset of a (Folio-validated) record.
pub fn preset_from_record(r: &StudioMotionPreset) -> MotionPreset {
    MotionPreset {
        name: r.name.clone(),
        effects: r.effects.iter().map(|e| PresetEffect { instance: e.instance_id.clone(), effect_kind: e.effect_kind.clone() }).collect(),
        entries: r
            .properties
            .iter()
            .map(|p| PresetEntry {
                path: p.path.clone(),
                kind: if p.insert { PresetEntryKind::Insert } else { PresetEntryKind::Write },
                static_value: p.value.static_value.clone(),
                keyframes: keyframes(&p.value),
                expression: p.expression.clone(),
            })
            .collect(),
    }
}

/// The stored record of a motion preset (save), with its id and category path.
pub fn record_from_preset(p: &MotionPreset, preset_id: &str, category_path: &[String]) -> StudioMotionPreset {
    StudioMotionPreset {
        preset_id: preset_id.to_owned(),
        name: p.name.clone(),
        category_path: category_path.to_vec(),
        effects: p.effects.iter().map(|e| StudioMotionPresetEffect { instance_id: e.instance.clone(), effect_kind: e.effect_kind.clone() }).collect(),
        properties: p
            .entries
            .iter()
            .map(|e| StudioMotionPresetProperty {
                path: e.path.clone(),
                insert: e.kind == PresetEntryKind::Insert,
                value: keyed(&e.static_value, &e.keyframes),
                expression: e.expression.clone(),
            })
            .collect(),
    }
}

/// Applies a stored preset to the layer at `root` with its keyframes starting at `at` (the
/// "apply at the current time" command): motion's all-or-nothing `apply_preset`, so a mismatch is
/// `PRESET_TARGET_INCOMPATIBLE` naming the unmatched paths and nothing is applied.
pub fn apply_preset_record(store: &mut PropertyStore, root: &str, record: &StudioMotionPreset, at: Ticks) -> Result<AppliedPreset, PresetError> {
    apply_preset(store, root, &preset_from_record(record), at)
}
