//! Animation presets on the property tree (lane SMB-AE-PARTICLES-AUDIO-PRESETS slice 3;
//! STU-MOT-110 "A property preset writes values, keyframes and expressions into named property
//! paths on the target"; STU-MOT-111 "Preset application is path-based and MUST fail loudly on a
//! mismatch ... PRESET_TARGET_INCOMPATIBLE ... applies nothing").
//!
//! A [`MotionPreset`] holds entries addressed RELATIVE to a target layer root (`/`-joined keys):
//! - [`PresetEntryKind::Write`] entries write an existing property of the target (transform,
//!   text-animator or other authored properties): static value, keyframes and expression;
//! - [`PresetEntryKind::Insert`] entries are the parameters of the preset's effect instances
//!   (`effects/<instance>/<param>`, STU-MOT-017: an effect instance is a property group) and are
//!   created on apply. An instance whose group already exists on the target is applied as
//!   `<instance>_2`, `_3`, ... (the returned [`AppliedPreset::instances`] maps each preset instance
//!   to its applied key so the effect-stack owner adds the matching stack entries).
//!
//! Keyframe times are relative to the preset origin; [`apply_preset`] shifts them by the
//! application time (`offset`, the current time). [`capture_preset`] records a selection with times
//! relative to `origin` (or to the earliest captured key when `origin` is `None`).
//!
//! Atomicity: every write target is resolved (missing path or a component-count mismatch is
//! unmatched), every replacement and new property is built and validated, the instance renames are
//! resolved and the graph node bound checked BEFORE the first mutation; a refusal leaves the store
//! and its revision unchanged. Expressions are installed with their text; their read sets are the
//! caller's to declare (`PropertyStore::set_reads`), as for any authored expression.
//! After Effects `.ffx` semantics are UNVERIFIED (D5); `.ffx` itself is never read (Folio refuses it).
use crate::depgraph::MAX_GRAPH_NODES;
use crate::error::MotionError;
use crate::keyframe::Keyframe;
use crate::path::{PropertyPath, validate_key};
use crate::property::Property;
use crate::store::PropertyStore;
use hsk_studio_accord::Ticks;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Most entries of one preset.
pub const MAX_PRESET_ENTRIES: usize = 4096;
/// Stable code of [`PresetError::TargetIncompatible`] (STU-MOT-111).
pub const PRESET_TARGET_INCOMPATIBLE: &str = "PRESET_TARGET_INCOMPATIBLE";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresetEntryKind {
    /// Writes an existing property of the target.
    Write,
    /// Creates a parameter of an effect instance (`effects/<instance>/<param>`).
    Insert,
}

/// One property of a preset.
#[derive(Clone, Debug, PartialEq)]
pub struct PresetEntry {
    /// Relative to the target layer root.
    pub path: String,
    pub kind: PresetEntryKind,
    pub static_value: Vec<f64>,
    /// Times relative to the preset origin.
    pub keyframes: Vec<Keyframe>,
    pub expression: Option<String>,
}

/// One effect instance a preset carries (its parameters are `Insert` entries under
/// `effects/<instance>/`); `effect_kind` is the effect-stack owner's key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetEffect {
    pub instance: String,
    pub effect_kind: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MotionPreset {
    pub name: String,
    pub effects: Vec<PresetEffect>,
    pub entries: Vec<PresetEntry>,
}

/// What [`apply_preset`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedPreset {
    pub written: Vec<PropertyPath>,
    pub inserted: Vec<PropertyPath>,
    /// Preset instance key -> applied instance key (renamed when the target already had it).
    pub instances: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PresetError {
    /// STU-MOT-111: the unmatched relative paths, nothing applied.
    TargetIncompatible { unmatched: Vec<String> },
    /// The preset itself is malformed (names the entry path or field).
    Invalid(String),
    Motion(MotionError),
}

impl PresetError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::TargetIncompatible { .. } => PRESET_TARGET_INCOMPATIBLE,
            Self::Invalid(_) => "preset_invalid",
            Self::Motion(e) => e.code(),
        }
    }
}

impl fmt::Display for PresetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetIncompatible { unmatched } => write!(f, "{}:{}", self.code(), unmatched.join(",")),
            Self::Invalid(t) => write!(f, "{}:{t}", self.code()),
            Self::Motion(_) => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for PresetError {}

impl From<MotionError> for PresetError {
    fn from(e: MotionError) -> Self {
        Self::Motion(e)
    }
}

fn join(root: &str, rel: &str) -> Result<PropertyPath, PresetError> {
    PropertyPath::new(&format!("{root}/{rel}")).map_err(|_| PresetError::Invalid(rel.to_owned()))
}

/// `effects/<instance>/<param...>` -> (instance, rest).
fn effect_parts(rel: &str) -> Option<(&str, &str)> {
    let rest = rel.strip_prefix("effects/")?;
    let (instance, param) = rest.split_once('/')?;
    (!instance.is_empty() && !param.is_empty()).then_some((instance, param))
}

fn shifted(keys: &[Keyframe], offset: Ticks, back: bool) -> Result<Vec<Keyframe>, PresetError> {
    keys.iter()
        .map(|k| {
            let t = if back { k.time.value().checked_sub(offset.value()) } else { k.time.value().checked_add(offset.value()) };
            let t = t.ok_or(PresetError::Motion(MotionError::TickOverflow))?;
            let mut k = k.clone();
            k.time = Ticks::new(t);
            Ok(k)
        })
        .collect()
}

/// Structural checks: entry count, unique relative paths, `Insert` entries exactly the parameters
/// of a declared instance and `Write` entries never under `effects/<declared instance>/`, unique
/// key-grammar instance keys.
pub fn validate_preset(preset: &MotionPreset) -> Result<(), PresetError> {
    if preset.entries.is_empty() || preset.entries.len() > MAX_PRESET_ENTRIES {
        return Err(PresetError::Invalid("entries".into()));
    }
    let mut instances = BTreeSet::new();
    for e in &preset.effects {
        validate_key(&e.instance).map_err(|_| PresetError::Invalid(format!("effects/{}", e.instance)))?;
        validate_key(&e.effect_kind).map_err(|_| PresetError::Invalid(format!("effects/{}/kind", e.instance)))?;
        if !instances.insert(e.instance.as_str()) {
            return Err(PresetError::Invalid(format!("effects/{}", e.instance)));
        }
    }
    let mut paths = BTreeSet::new();
    for e in &preset.entries {
        PropertyPath::new(&e.path).map_err(|_| PresetError::Invalid(e.path.clone()))?;
        if !paths.insert(e.path.as_str()) {
            return Err(PresetError::Invalid(e.path.clone()));
        }
        let declared = effect_parts(&e.path).is_some_and(|(i, _)| instances.contains(i));
        if (e.kind == PresetEntryKind::Insert) != declared {
            return Err(PresetError::Invalid(e.path.clone()));
        }
    }
    Ok(())
}

/// Records the properties at `root/<rel>` for every `rel` in `paths` (`effects/<instance>/...`
/// paths of a listed `effects` instance become `Insert` entries, the rest `Write`). Times are made
/// relative to `origin`, or to the earliest captured key when `None` (0 without keys); a key before
/// the origin is refused. A missing path is `UnknownProperty`.
pub fn capture_preset(
    store: &PropertyStore,
    root: &str,
    name: &str,
    effects: &[PresetEffect],
    paths: &[&str],
    origin: Option<Ticks>,
) -> Result<MotionPreset, PresetError> {
    let mut props = Vec::with_capacity(paths.len());
    for rel in paths {
        let p = store.get(&join(root, rel)?).ok_or(PresetError::Motion(MotionError::UnknownProperty))?;
        props.push((*rel, p));
    }
    let origin = origin.unwrap_or_else(|| {
        Ticks::new(props.iter().filter_map(|(_, p)| p.keyframes().first().map(|k| k.time.value())).min().unwrap_or(0))
    });
    let instances: BTreeSet<&str> = effects.iter().map(|e| e.instance.as_str()).collect();
    let mut entries = Vec::with_capacity(props.len());
    for (rel, p) in props {
        let insert = effect_parts(rel).is_some_and(|(i, _)| instances.contains(i));
        entries.push(PresetEntry {
            path: rel.to_owned(),
            kind: if insert { PresetEntryKind::Insert } else { PresetEntryKind::Write },
            static_value: p.static_value().to_vec(),
            keyframes: shifted(p.keyframes(), origin, true)?,
            expression: p.expression().map(str::to_owned),
        });
    }
    let preset = MotionPreset { name: name.to_owned(), effects: effects.to_vec(), entries };
    validate_preset(&preset)?;
    Ok(preset)
}

fn build(key: &str, e: &PresetEntry, base: Option<&Property>, offset: Ticks) -> Result<Property, PresetError> {
    let keys = shifted(&e.keyframes, offset, false)?;
    let mut p = match base {
        Some(b) => {
            let mut p = b.clone();
            p.set_static_value(e.static_value.clone())?;
            p.set_keyframes(keys)?;
            p
        }
        None => Property::new(key, e.static_value.clone(), keys)?,
    };
    p.set_expression(e.expression.as_deref())?;
    Ok(p)
}

/// Applies `preset` to the layer at `root` with keyframes shifted by `offset` (module docs).
pub fn apply_preset(store: &mut PropertyStore, root: &str, preset: &MotionPreset, offset: Ticks) -> Result<AppliedPreset, PresetError> {
    validate_preset(preset)?;
    // 1. Write targets: every one must exist with the same component count.
    let mut unmatched = Vec::new();
    for e in preset.entries.iter().filter(|e| e.kind == PresetEntryKind::Write) {
        match store.get(&join(root, &e.path)?) {
            Some(p) if p.static_value().len() == e.static_value.len() => {}
            _ => unmatched.push(e.path.clone()),
        }
    }
    if !unmatched.is_empty() {
        return Err(PresetError::TargetIncompatible { unmatched });
    }
    // 2. Instance renames: the first free `<instance>`, `<instance>_2`, ... on the target.
    let taken = |key: &str| store_has_group(store, &format!("{root}/effects/{key}/"));
    let mut instances = BTreeMap::new();
    let mut used: BTreeSet<String> = BTreeSet::new();
    for fx in &preset.effects {
        let mut n = 1u32;
        let mut key = fx.instance.clone();
        while taken(&key) || used.contains(&key) {
            n = n.checked_add(1).ok_or(PresetError::Invalid(fx.instance.clone()))?;
            key = format!("{}_{n}", fx.instance);
            validate_key(&key).map_err(|_| PresetError::Invalid(fx.instance.clone()))?;
        }
        used.insert(key.clone());
        instances.insert(fx.instance.clone(), key);
    }
    // 3. Build every replacement and new property.
    let mut writes = Vec::new();
    let mut inserts = Vec::new();
    for e in &preset.entries {
        match e.kind {
            PresetEntryKind::Write => {
                let path = join(root, &e.path)?;
                let base = store.get(&path).ok_or(PresetError::Motion(MotionError::UnknownProperty))?;
                writes.push((path, build(base.key(), e, Some(base), offset)?));
            }
            PresetEntryKind::Insert => {
                let (instance, param) = effect_parts(&e.path).ok_or_else(|| PresetError::Invalid(e.path.clone()))?;
                let applied = instances.get(instance).ok_or_else(|| PresetError::Invalid(e.path.clone()))?;
                let path = join(root, &format!("effects/{applied}/{param}"))?;
                if store.get(&path).is_some() {
                    return Err(PresetError::Motion(MotionError::DuplicateProperty));
                }
                let key = param.rsplit('/').next().unwrap_or(param);
                inserts.push((path, build(key, e, None, offset)?));
            }
        }
    }
    if store.len().saturating_add(inserts.len()) > MAX_GRAPH_NODES {
        return Err(PresetError::Motion(MotionError::NodeLimit));
    }
    // 4. Mutate (nothing below can fail on a checked state).
    let mut applied = AppliedPreset { written: Vec::new(), inserted: Vec::new(), instances };
    for (path, p) in writes {
        store.update(&path, |slot| {
            *slot = p;
            Ok(())
        })?;
        applied.written.push(path);
    }
    for (path, p) in inserts {
        store.insert(path.clone(), p)?;
        applied.inserted.push(path);
    }
    Ok(applied)
}

/// True when any property of the store lives under `prefix` (an existing effect group).
fn store_has_group(store: &PropertyStore, prefix: &str) -> bool {
    store.graph().eval_order().iter().any(|p| p.as_str().starts_with(prefix))
}
