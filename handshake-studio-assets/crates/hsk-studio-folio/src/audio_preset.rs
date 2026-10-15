//! Audio effect presets as stored records (lane SMB-AUDITION-SESSION; STU-FX-039 the
//! `StudioStyleRegistry`, STU-FX-142 "an effect preset is an ordered `StudioEffectStack` fragment:
//! one or more effect instances with their parameter values", STU-FX-142a per-property keyframes
//! carried alongside static values, STU-FX-143 a category path rather than a flat name).
//!
//! [`StudioAudioEffectPresetSet`] holds the audio effect preset entries of a style registry
//! (`schema_id` = `hsk.studio.style_registry@1`); the registry's other members (effect styles,
//! publishing, live references) are not stored here. Folio validates structure, identity, counts and
//! finiteness; which `filter_kind` keys exist and what their parameters mean belongs to the audio
//! engine adapter that applies a preset (it refuses unknown kinds and parameters). Presets reference
//! no assets, so STU-FX-144's content-addressed asset references do not arise for these records.
//!
//! The records arrive from untrusted wire JSON ([`parse_audio_effect_presets`]: the shared Folio
//! preflight, duplicate keys refused): integer arithmetic is denied (V4-01), every count is checked
//! before use and charged against the [`Budget`].
#![deny(clippy::arithmetic_side_effects)]
use super::{Budget, Code, Result, Unresolved, err, key, values::Ctx};
use hsk_studio_accord::{CancellationToken, SCHEMA_STUDIO_STYLE_REGISTRY};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Presets one set may hold (authored bound).
pub const MAX_AUDIO_PRESETS: usize = 4096;
/// Effect instances one preset may hold (score's widest chain).
pub const MAX_AUDIO_PRESET_EFFECTS: usize = 64;
/// Parameters one instance may hold (authored bound).
pub const MAX_AUDIO_PRESET_PARAMS: usize = 256;
/// Keyframes one instance may hold (score's automation lane bound).
pub const MAX_AUDIO_PRESET_KEYFRAMES: usize = 4096;
/// Category path depth (authored bound; the captured paths are at most two deep).
pub const MAX_AUDIO_PRESET_CATEGORY_DEPTH: usize = 8;
/// Longest name or category segment in bytes.
pub const MAX_AUDIO_PRESET_TEXT: usize = 256;

/// One parameter value of an effect instance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAudioPresetParam {
    /// Parameter key (key grammar), unique within the instance.
    pub key: String,
    pub value: f64,
}

/// One keyframe of a parameter: `offset_ticks` from the start of the preset's target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAudioPresetKeyframe {
    /// A key of the instance's `params`.
    pub key: String,
    pub offset_ticks: u64,
    pub value: f64,
}

/// One effect instance of a preset (stack order = list order).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAudioEffectInstance {
    /// Unique within the preset (key grammar).
    pub instance_id: String,
    /// Audio processor key (key grammar), interpreted by the applying engine adapter.
    pub filter_kind: String,
    pub enabled: bool,
    pub params: Vec<StudioAudioPresetParam>,
    /// Per-parameter keyframes; absent is not encoded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<StudioAudioPresetKeyframe>,
}

/// One named audio effect preset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAudioEffectPreset {
    /// Unique within the set (key grammar).
    pub preset_id: String,
    pub name: String,
    /// Browsable category path, outermost first (STU-FX-143); empty = top level.
    pub category_path: Vec<String>,
    pub effects: Vec<StudioAudioEffectInstance>,
}

/// The audio effect preset entries of a style registry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAudioEffectPresetSet {
    /// `hsk.studio.style_registry@1`.
    pub schema_id: String,
    pub presets: Vec<StudioAudioEffectPreset>,
}

impl StudioAudioEffectPresetSet {
    /// An empty set.
    pub fn new() -> Self {
        Self {
            schema_id: SCHEMA_STUDIO_STYLE_REGISTRY.into(),
            presets: Vec::new(),
        }
    }
}

impl Default for StudioAudioEffectPresetSet {
    fn default() -> Self {
        Self::new()
    }
}

fn invalid(target: &str, field: &str) -> super::Diagnostic {
    err(Code::InvalidField, format!("{target}/{field}"))
}

fn text(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= MAX_AUDIO_PRESET_TEXT && !s.chars().any(char::is_control)
}

pub(crate) fn instance(e: &StudioAudioEffectInstance, target: &str, cx: &mut Ctx) -> Result<()> {
    cx.tick()?;
    if !key(&e.filter_kind) {
        return Err(invalid(target, "filter_kind"));
    }
    if e.params.len() > MAX_AUDIO_PRESET_PARAMS || e.keyframes.len() > MAX_AUDIO_PRESET_KEYFRAMES {
        return Err(err(Code::BudgetExceeded, target));
    }
    cx.nodes(e.params.len(), target)?;
    cx.nodes(e.keyframes.len(), target)?;
    let mut keys = BTreeSet::new();
    for p in &e.params {
        if !key(&p.key) {
            return Err(invalid(target, "params/key"));
        }
        if !keys.insert(p.key.as_str()) {
            return Err(err(Code::DuplicateField, format!("{target}/params/{}", p.key)));
        }
        if !p.value.is_finite() {
            return Err(invalid(target, "params/value"));
        }
    }
    let mut last: BTreeMap<&str, u64> = BTreeMap::new();
    for k in &e.keyframes {
        cx.tick()?;
        if !keys.contains(k.key.as_str()) {
            return Err(invalid(target, "keyframes/key"));
        }
        if !k.value.is_finite() {
            return Err(invalid(target, "keyframes/value"));
        }
        if last.get(k.key.as_str()).is_some_and(|t| *t >= k.offset_ticks) {
            return Err(invalid(target, "keyframes/offset_ticks"));
        }
        last.insert(k.key.as_str(), k.offset_ticks);
    }
    Ok(())
}

/// Structural validation: schema id; preset count; unique key-grammar preset ids; names and category
/// segments non-blank, at most [`MAX_AUDIO_PRESET_TEXT`] bytes, no control characters; category
/// depth; 1..=[`MAX_AUDIO_PRESET_EFFECTS`] effects with unique key-grammar instance ids and
/// key-grammar `filter_kind`; unique parameter keys with finite values; keyframes naming a parameter
/// of their instance, finite, strictly increasing in `offset_ticks` per parameter. Every preset,
/// instance, parameter and keyframe counts against `Budget::nodes`, every text against
/// `Budget::payload_bytes`.
pub fn validate_audio_effect_presets(set: &StudioAudioEffectPresetSet, b: Budget, token: &CancellationToken) -> Result<()> {
    let mut cx = Ctx::new(b, token, &Unresolved);
    cx.tick()?;
    if set.schema_id != SCHEMA_STUDIO_STYLE_REGISTRY {
        return Err(invalid("audio_presets", "schema_id"));
    }
    if set.presets.len() > MAX_AUDIO_PRESETS {
        return Err(err(Code::BudgetExceeded, "audio_presets"));
    }
    cx.nodes(set.presets.len(), "audio_presets")?;
    let mut ids = BTreeSet::new();
    for p in &set.presets {
        cx.tick()?;
        if !key(&p.preset_id) || !ids.insert(p.preset_id.as_str()) {
            return Err(err(Code::InvalidId, format!("audio_presets/{}", p.preset_id)));
        }
        let target = format!("audio_presets/{}", p.preset_id);
        if !text(&p.name) {
            return Err(invalid(&target, "name"));
        }
        cx.text(&p.name, &target)?;
        if p.category_path.len() > MAX_AUDIO_PRESET_CATEGORY_DEPTH {
            return Err(invalid(&target, "category_path"));
        }
        for c in &p.category_path {
            if !text(c) {
                return Err(invalid(&target, "category_path"));
            }
            cx.text(c, &target)?;
        }
        if p.effects.is_empty() || p.effects.len() > MAX_AUDIO_PRESET_EFFECTS {
            return Err(invalid(&target, "effects"));
        }
        cx.nodes(p.effects.len(), &target)?;
        let mut instances = BTreeSet::new();
        for e in &p.effects {
            if !key(&e.instance_id) || !instances.insert(e.instance_id.as_str()) {
                return Err(invalid(&target, "effects/instance_id"));
            }
            instance(e, &format!("{target}/{}", e.instance_id), &mut cx)?;
        }
    }
    Ok(())
}

/// Parses and validates a preset set from wire JSON (shared Folio preflight: `Budget::input_bytes`
/// and depth; duplicate object keys are `DuplicateField`; unknown fields and wrong types
/// `InvalidJson`).
pub fn parse_audio_effect_presets(input: &[u8], b: Budget, token: &CancellationToken) -> Result<StudioAudioEffectPresetSet> {
    super::preflight(input, b, token)?;
    let value = serde_json::from_slice::<super::UniqueValue>(input)
        .map_err(|e| {
            err(
                if e.to_string().contains("duplicate_field") { Code::DuplicateField } else { Code::InvalidJson },
                "audio_presets",
            )
        })?
        .0;
    let set: StudioAudioEffectPresetSet = serde_json::from_value(value).map_err(|_| err(Code::InvalidJson, "audio_presets"))?;
    validate_audio_effect_presets(&set, b, token)?;
    Ok(set)
}
