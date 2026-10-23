//! Animation (motion) presets as stored records (lane SMB-AE-PARTICLES-AUDIO-PRESETS slice 3;
//! STU-MOT-110 "A motion preset is a `StudioStyleRegistry` entry ... A property preset writes
//! values, keyframes and expressions into named property paths on the target ... A preset format
//! that carries only static values cannot represent this material"; STU-FX-142 an effect preset is
//! an ordered effect-instance fragment; STU-FX-143 a category path).
//!
//! [`StudioMotionPresetSet`] holds the motion preset entries of a style registry (`schema_id` =
//! `hsk.studio.style_registry@1`): per preset its effect instances (instance key + effect kind, in
//! stack order) and its properties, each a RELATIVE property path under the target layer root, an
//! `insert` flag (true = a parameter of one of the preset's effect instances, created on apply,
//! path `effects/<instance_id>/<param>`), a keyed value with the composition keyframe rules
//! (`StudioKeyedValue`; times relative to the preset origin) and an optional expression. Folio
//! validates structure and bounds; motion applies (path match, renames, time offset) and the
//! composite adapter converts.
//!
//! This is the Studio-native preset format. After Effects `.ffx` is a proprietary binary (a RIFX
//! container with an `FaFX` form type): [`parse_motion_presets`] refuses it by its magic BEFORE any
//! JSON parsing with `Code::UnsupportedSchema` at target [`MOTION_PRESET_FFX`]; it is never read.
#![deny(clippy::arithmetic_side_effects)]
use super::composition::validate_keyed;
use super::{Budget, Code, Result, StudioKeyedValue, Unresolved, err, key, values::Ctx};
use hsk_studio_accord::{CancellationToken, SCHEMA_STUDIO_STYLE_REGISTRY};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Presets one set may hold.
pub const MAX_MOTION_PRESETS: usize = 4096;
/// Effect instances one preset may hold.
pub const MAX_MOTION_PRESET_EFFECTS: usize = 64;
/// Properties one preset may hold (motion `MAX_PRESET_ENTRIES`).
pub const MAX_MOTION_PRESET_PROPERTIES: usize = 4096;
/// Components of one property value (motion `MAX_DIMENSION`).
pub const MAX_MOTION_PRESET_DIMENSION: usize = 64;
/// Longest relative path in bytes (motion `MAX_PATH_BYTES`).
pub const MAX_MOTION_PRESET_PATH: usize = 256;
/// Longest expression in bytes (motion `MAX_EXPRESSION_BYTES`).
pub const MAX_MOTION_PRESET_EXPRESSION: usize = 65_536;
/// Longest name or category segment in bytes.
pub const MAX_MOTION_PRESET_TEXT: usize = 256;
/// Category path depth.
pub const MAX_MOTION_PRESET_CATEGORY_DEPTH: usize = 8;
/// Largest absolute component value.
pub const MAX_MOTION_PRESET_ABS_VALUE: f64 = 1.0e12;
/// The refusal target of an After Effects `.ffx` input (`Code::UnsupportedSchema`).
pub const MOTION_PRESET_FFX: &str = "motion_presets/ffx_proprietary";

/// One effect instance of a preset (stack order = list order).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMotionPresetEffect {
    /// Unique within the preset (key grammar).
    pub instance_id: String,
    /// Effect key (key grammar), interpreted by the effect-stack owner that applies it.
    pub effect_kind: String,
}

/// One property of a preset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMotionPresetProperty {
    /// `/`-joined key segments relative to the target layer root, unique within the preset.
    pub path: String,
    /// A parameter of one of the preset's effect instances (`effects/<instance_id>/<param>`).
    pub insert: bool,
    /// Static value and keyframes (times relative to the preset origin).
    pub value: StudioKeyedValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<String>,
}

/// One named motion preset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMotionPreset {
    /// Unique within the set (key grammar).
    pub preset_id: String,
    pub name: String,
    /// Browsable category path, outermost first (STU-FX-143).
    pub category_path: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<StudioMotionPresetEffect>,
    pub properties: Vec<StudioMotionPresetProperty>,
}

/// The motion preset entries of a style registry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioMotionPresetSet {
    /// `hsk.studio.style_registry@1`.
    pub schema_id: String,
    pub presets: Vec<StudioMotionPreset>,
}

impl StudioMotionPresetSet {
    pub fn new() -> Self {
        Self { schema_id: SCHEMA_STUDIO_STYLE_REGISTRY.into(), presets: Vec::new() }
    }
}

impl Default for StudioMotionPresetSet {
    fn default() -> Self {
        Self::new()
    }
}

fn invalid(target: &str, field: &str) -> super::Diagnostic {
    err(Code::InvalidField, format!("{target}/{field}"))
}

fn text(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= MAX_MOTION_PRESET_TEXT && !s.chars().any(char::is_control)
}

/// A relative property path: non-empty `/`-joined segments, no `.`/`..`, no control characters.
fn rel_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= MAX_MOTION_PRESET_PATH
        && !p.chars().any(char::is_control)
        && p.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
}

/// True for an After Effects `.ffx` byte stream (RIFX container, `FaFX` form type).
pub fn is_ffx(input: &[u8]) -> bool {
    input.len() >= 12 && &input[..4] == b"RIFX" && &input[8..12] == b"FaFX"
}

/// Structural validation: schema id; counts; unique key-grammar preset ids; names and category
/// segments; unique key-grammar instance ids and effect kinds; per property a valid unique relative
/// path, `insert` exactly when the path is `effects/<declared instance>/<param>`, a value of 1..=64
/// finite components within [`MAX_MOTION_PRESET_ABS_VALUE`] under the composition keyframe rules
/// (`validate_keyed`), and an expression of at most [`MAX_MOTION_PRESET_EXPRESSION`] bytes. Every
/// preset, instance, property and keyframe counts against `Budget::nodes`, every text against
/// `Budget::payload_bytes`.
pub fn validate_motion_presets(set: &StudioMotionPresetSet, b: Budget, token: &CancellationToken) -> Result<()> {
    let mut cx = Ctx::new(b, token, &Unresolved);
    cx.tick()?;
    if set.schema_id != SCHEMA_STUDIO_STYLE_REGISTRY {
        return Err(invalid("motion_presets", "schema_id"));
    }
    if set.presets.len() > MAX_MOTION_PRESETS {
        return Err(err(Code::BudgetExceeded, "motion_presets"));
    }
    cx.nodes(set.presets.len(), "motion_presets")?;
    let mut ids = BTreeSet::new();
    for p in &set.presets {
        cx.tick()?;
        if !key(&p.preset_id) || !ids.insert(p.preset_id.as_str()) {
            return Err(err(Code::InvalidId, format!("motion_presets/{}", p.preset_id)));
        }
        let target = format!("motion_presets/{}", p.preset_id);
        if !text(&p.name) {
            return Err(invalid(&target, "name"));
        }
        cx.text(&p.name, &target)?;
        if p.category_path.len() > MAX_MOTION_PRESET_CATEGORY_DEPTH {
            return Err(invalid(&target, "category_path"));
        }
        for c in &p.category_path {
            if !text(c) {
                return Err(invalid(&target, "category_path"));
            }
            cx.text(c, &target)?;
        }
        if p.effects.len() > MAX_MOTION_PRESET_EFFECTS {
            return Err(err(Code::BudgetExceeded, format!("{target}/effects")));
        }
        if p.properties.is_empty() || p.properties.len() > MAX_MOTION_PRESET_PROPERTIES {
            return Err(invalid(&target, "properties"));
        }
        cx.nodes(p.effects.len(), &target)?;
        cx.nodes(p.properties.len(), &target)?;
        let mut instances = BTreeSet::new();
        for e in &p.effects {
            if !key(&e.instance_id) || !instances.insert(e.instance_id.as_str()) {
                return Err(invalid(&target, "effects/instance_id"));
            }
            if !key(&e.effect_kind) {
                return Err(invalid(&target, "effects/effect_kind"));
            }
        }
        let mut paths = BTreeSet::new();
        for prop in &p.properties {
            cx.tick()?;
            if !rel_path(&prop.path) || !paths.insert(prop.path.as_str()) {
                return Err(invalid(&target, "properties/path"));
            }
            cx.text(&prop.path, &target)?;
            let declared = prop
                .path
                .strip_prefix("effects/")
                .and_then(|r| r.split_once('/'))
                .is_some_and(|(i, rest)| !rest.is_empty() && instances.contains(i));
            if prop.insert != declared {
                return Err(invalid(&format!("{target}/{}", prop.path), "insert"));
            }
            let dim = prop.value.static_value.len();
            let pt = format!("{target}/{}", prop.path);
            if dim == 0 || dim > MAX_MOTION_PRESET_DIMENSION {
                return Err(invalid(&pt, "value"));
            }
            let ok = |x: &[f64]| x.len() == dim && x.iter().all(|v| v.is_finite() && v.abs() <= MAX_MOTION_PRESET_ABS_VALUE);
            validate_keyed(&pt, dim, &ok, &prop.value.static_value, &prop.value.keyframes, &mut cx)?;
            if let Some(x) = &prop.expression {
                if x.len() > MAX_MOTION_PRESET_EXPRESSION {
                    return Err(invalid(&pt, "expression"));
                }
                cx.text(x, &pt)?;
            }
        }
    }
    Ok(())
}

/// Parses and validates a preset set from wire JSON. An After Effects `.ffx` input ([`is_ffx`]) is
/// refused first with `Code::UnsupportedSchema` at [`MOTION_PRESET_FFX`]; otherwise the shared Folio
/// preflight (`Budget::input_bytes`, depth), duplicate object keys `DuplicateField`, unknown fields
/// and wrong types `InvalidJson`, then [`validate_motion_presets`].
pub fn parse_motion_presets(input: &[u8], b: Budget, token: &CancellationToken) -> Result<StudioMotionPresetSet> {
    if is_ffx(input) {
        return Err(err(Code::UnsupportedSchema, MOTION_PRESET_FFX));
    }
    super::preflight(input, b, token)?;
    let value = serde_json::from_slice::<super::UniqueValue>(input)
        .map_err(|e| {
            err(
                if e.to_string().contains("duplicate_field") { Code::DuplicateField } else { Code::InvalidJson },
                "motion_presets",
            )
        })?
        .0;
    let set: StudioMotionPresetSet = serde_json::from_value(value).map_err(|_| err(Code::InvalidJson, "motion_presets"))?;
    validate_motion_presets(&set, b, token)?;
    Ok(set)
}
