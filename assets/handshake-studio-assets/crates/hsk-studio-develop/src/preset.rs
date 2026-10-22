//! Develop presets, snapshots and creative looks (lane SMB-DEVELOP-LENS-PRESETS slice 4;
//! STU-RAW-128 preset / snapshot registry, STU-RAW-126 creative profiles and look tables,
//! STU-RAW-013 preset amount).
//!
//! A preset is a PARTIAL parameter set that records exactly which keys it sets (a preset holding the
//! full state could not be applied on top of an edit, STU-RAW-128); a snapshot is a complete recipe.
//! Keys are this crate's stable develop parameter keys ([`preset_keys`]); applying a preset with an
//! amount `a` (0..=2) moves each numeric parameter to `base + a (preset - base)` and sets flags and
//! enumerations when `a > 0`; the result is validated as a whole (atomic: the base is never changed,
//! a refused result returns the typed error). [`DevelopPreset::merge`] layers presets (the later
//! preset wins per key).
//!
//! Text format (`hsk-develop-preset 1`, one record per line; this crate's own documented format):
//! ```text
//! hsk-develop-preset 1
//! meta name Warm Matte          # meta <field> <text to the end of the line>
//! flag supports_amount true     # capability flags of STU-RAW-126
//! set contrast -12
//! set enable.tone_curve true
//! set geometry.upright vertical
//! ```
//! Hostile input: byte cap, line cap, setting cap, unknown keys / fields / kinds refused, duplicate
//! keys refused, finite numbers only; every refusal is `invalid_preset`.
#![deny(clippy::arithmetic_side_effects)]

use crate::DevelopError;
use crate::adjust::BAND_NAMES;
use crate::effects::VignetteStyle;
use crate::geometry::UprightMode;
use crate::recipe::DevelopRecipe;
use std::collections::BTreeMap;

pub const MAX_PRESET_BYTES: usize = 1 << 20;
pub const MAX_PRESET_SETTINGS: usize = 4096;
const MAX_LINE: usize = 1024;
const MAX_META: usize = 256;

/// The STU-RAW-128 metadata fields.
pub const META_FIELDS: [&str; 10] =
    ["name", "short_name", "sort_name", "group", "cluster", "uuid", "description", "copyright", "contact_info", "preset_type"];
/// The STU-RAW-126 capability flags plus `is_stub`.
pub const FLAG_FIELDS: [&str; 10] = [
    "supports_amount",
    "supports_colour",
    "supports_monochrome",
    "supports_output_referred",
    "supports_scene_referred",
    "supports_high_dynamic_range",
    "supports_normal_dynamic_range",
    "show_in_presets",
    "show_in_quick_actions",
    "is_stub",
];

#[derive(Clone, Debug, PartialEq)]
pub enum PresetValue {
    Number(f64),
    Flag(bool),
    Text(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DevelopPreset {
    /// [`META_FIELDS`] -> text.
    pub metadata: BTreeMap<String, String>,
    /// [`FLAG_FIELDS`] -> value.
    pub flags: BTreeMap<String, bool>,
    /// `camera_model_restriction` (STU-RAW-126), when the preset is limited to cameras.
    pub camera_model_restriction: Option<String>,
    /// Parameter key -> value; exactly the parameters the preset sets.
    pub settings: BTreeMap<String, PresetValue>,
}

/// A complete named develop state (STU-RAW-128 snapshot).
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub name: String,
    pub recipe: DevelopRecipe,
}

enum Slot<'a> {
    Num(&'a mut f64),
    Flag(&'a mut bool),
    Seed(&'a mut u32),
    Upright(&'a mut UprightMode),
    Style(&'a mut VignetteStyle),
}

const WHEELS: [&str; 4] = ["shadows", "midtones", "highlights", "global"];

fn band(name: &str) -> Option<usize> {
    BAND_NAMES.iter().position(|b| *b == name)
}

fn slot<'a>(r: &'a mut DevelopRecipe, key: &str) -> Option<Slot<'a>> {
    use Slot::{Flag, Num};
    let a = &mut r.adjust;
    let parts: Vec<&str> = key.split('.').collect();
    Some(match parts.as_slice() {
        ["exposure"] => Num(&mut r.exposure_stops),
        ["temperature"] => Num(&mut a.white_balance.temperature),
        ["tint"] => Num(&mut a.white_balance.tint),
        ["contrast"] => Num(&mut a.tone.contrast),
        ["highlights"] => Num(&mut a.tone.highlights),
        ["shadows"] => Num(&mut a.tone.shadows),
        ["whites"] => Num(&mut a.tone.whites),
        ["blacks"] => Num(&mut a.tone.blacks),
        ["vibrance"] => Num(&mut a.presence.vibrance),
        ["saturation"] => Num(&mut a.presence.saturation),
        ["clarity"] => Num(&mut a.presence.clarity),
        ["texture"] => Num(&mut a.detail.texture),
        ["dehaze"] => Num(&mut a.detail.dehaze),
        ["sharpen_amount"] => Num(&mut a.detail.sharpen.amount),
        ["sharpen_radius"] => Num(&mut a.detail.sharpen.radius),
        ["sharpen_detail"] => Num(&mut a.detail.sharpen.detail),
        ["sharpen_masking"] => Num(&mut a.detail.sharpen.masking),
        ["luminance_noise"] => Num(&mut a.detail.noise.luminance),
        ["colour_noise"] => Num(&mut a.detail.noise.colour),
        ["mixer", b, f] => {
            let band = &mut a.mixer.bands[band(b)?];
            match *f {
                "hue" => Num(&mut band.hue),
                "saturation" => Num(&mut band.saturation),
                "luminance" => Num(&mut band.luminance),
                _ => return None,
            }
        }
        ["parametric", f] => {
            let p = &mut a.parametric;
            Num(match *f {
                "highlights" => &mut p.highlights,
                "lights" => &mut p.lights,
                "darks" => &mut p.darks,
                "shadows" => &mut p.shadows,
                "shadow_split" => &mut p.shadow_split,
                "midtone_split" => &mut p.midtone_split,
                "highlight_split" => &mut p.highlight_split,
                _ => return None,
            })
        }
        ["grayscale", "convert"] => Flag(&mut a.grayscale.convert),
        ["grayscale", b] => Num(&mut a.grayscale.bands[band(b)?]),
        ["grading", "blending"] => Num(&mut a.grading.blending),
        ["grading", "balance"] => Num(&mut a.grading.balance),
        ["grading", w, f] => {
            let g = &mut a.grading;
            let wheel = match *w {
                "shadows" => &mut g.shadows,
                "midtones" => &mut g.midtones,
                "highlights" => &mut g.highlights,
                "global" => &mut g.global,
                _ => return None,
            };
            Num(match *f {
                "hue" => &mut wheel.hue,
                "saturation" => &mut wheel.saturation,
                "luminance" => &mut wheel.luminance,
                _ => return None,
            })
        }
        ["optics", f] => {
            let o = &mut a.optics;
            match *f {
                "profile_enable" => Flag(&mut o.lens_profile_enable),
                "auto_ca" => Flag(&mut o.auto_lateral_chromatic_aberration),
                "distortion_scale" => Num(&mut o.lens_profile_distortion_scale),
                "vignetting_scale" => Num(&mut o.lens_profile_vignetting_scale),
                "manual_distortion" => Num(&mut o.manual_distortion_amount),
                "manual_vignette" => Num(&mut o.manual_vignette_amount),
                _ => return None,
            }
        }
        ["optics", "defringe", f] => {
            let d = &mut a.optics.defringe;
            Num(match *f {
                "purple_amount" => &mut d.purple_amount,
                "purple_hue_low" => &mut d.purple_hue_low,
                "purple_hue_high" => &mut d.purple_hue_high,
                "green_amount" => &mut d.green_amount,
                "green_hue_low" => &mut d.green_hue_low,
                "green_hue_high" => &mut d.green_hue_high,
                _ => return None,
            })
        }
        ["geometry", f] => {
            let g = &mut a.geometry;
            match *f {
                "vertical" => Num(&mut g.perspective_vertical),
                "horizontal" => Num(&mut g.perspective_horizontal),
                "rotate" => Num(&mut g.perspective_rotate),
                "scale" => Num(&mut g.perspective_scale),
                "aspect" => Num(&mut g.perspective_aspect),
                "offset_x" => Num(&mut g.perspective_offset_x),
                "offset_y" => Num(&mut g.perspective_offset_y),
                "upright" => Slot::Upright(&mut g.upright_mode),
                _ => return None,
            }
        }
        ["effects", "grain", f] => {
            let g = &mut a.effects.grain;
            match *f {
                "amount" => Num(&mut g.amount),
                "size" => Num(&mut g.size),
                "frequency" => Num(&mut g.frequency),
                "seed" => Slot::Seed(&mut g.seed),
                _ => return None,
            }
        }
        ["effects", "vignette", f] => {
            let v = &mut a.effects.vignette;
            match *f {
                "amount" => Num(&mut v.amount),
                "midpoint" => Num(&mut v.midpoint),
                "roundness" => Num(&mut v.roundness),
                "feather" => Num(&mut v.feather),
                "highlight_contrast" => Num(&mut v.highlight_contrast),
                "style" => Slot::Style(&mut v.style),
                _ => return None,
            }
        }
        ["calibration", f] => {
            let c = &mut a.calibration;
            Num(match *f {
                "shadow_tint" => &mut c.shadow_tint,
                "red_hue" => &mut c.red_hue,
                "red_saturation" => &mut c.red_saturation,
                "green_hue" => &mut c.green_hue,
                "green_saturation" => &mut c.green_saturation,
                "blue_hue" => &mut c.blue_hue,
                "blue_saturation" => &mut c.blue_saturation,
                _ => return None,
            })
        }
        ["look", "amount"] => Num(&mut a.look.as_mut()?.amount),
        ["enable", f] => {
            let e = &mut r.enables;
            Flag(match *f {
                "calibration" => &mut e.enable_calibration,
                "colour_adjustments" => &mut e.enable_colour_adjustments,
                "detail" => &mut e.enable_detail,
                "effects" => &mut e.enable_effects,
                "grayscale_mix" => &mut e.enable_grayscale_mix,
                "lens_corrections" => &mut e.enable_lens_corrections,
                "mask_group_based_corrections" => &mut e.enable_mask_group_based_corrections,
                "red_eye" => &mut e.enable_red_eye,
                "retouch" => &mut e.enable_retouch,
                "tone_curve" => &mut e.enable_tone_curve,
                "transform" => &mut e.enable_transform,
                "distraction_removal" => &mut e.enable_distraction_removal,
                _ => return None,
            })
        }
        _ => return None,
    })
}

/// Every parameter key a preset may set (stable; `look.amount` only applies while a look is set).
pub fn preset_keys() -> Vec<String> {
    let mut keys: Vec<String> = [
        "exposure", "temperature", "tint", "contrast", "highlights", "shadows", "whites", "blacks", "vibrance", "saturation",
        "clarity", "texture", "dehaze", "sharpen_amount", "sharpen_radius", "sharpen_detail", "sharpen_masking",
        "luminance_noise", "colour_noise",
    ]
    .iter()
    .map(|k| (*k).to_owned())
    .collect();
    for b in BAND_NAMES {
        for f in ["hue", "saturation", "luminance"] {
            keys.push(format!("mixer.{b}.{f}"));
        }
    }
    for f in ["highlights", "lights", "darks", "shadows", "shadow_split", "midtone_split", "highlight_split"] {
        keys.push(format!("parametric.{f}"));
    }
    keys.push("grayscale.convert".into());
    for b in BAND_NAMES {
        keys.push(format!("grayscale.{b}"));
    }
    for w in WHEELS {
        for f in ["hue", "saturation", "luminance"] {
            keys.push(format!("grading.{w}.{f}"));
        }
    }
    keys.extend(["grading.blending", "grading.balance"].map(String::from));
    for f in ["profile_enable", "auto_ca", "distortion_scale", "vignetting_scale", "manual_distortion", "manual_vignette"] {
        keys.push(format!("optics.{f}"));
    }
    for f in ["purple_amount", "purple_hue_low", "purple_hue_high", "green_amount", "green_hue_low", "green_hue_high"] {
        keys.push(format!("optics.defringe.{f}"));
    }
    for f in ["vertical", "horizontal", "rotate", "scale", "aspect", "offset_x", "offset_y", "upright"] {
        keys.push(format!("geometry.{f}"));
    }
    for f in ["amount", "size", "frequency", "seed"] {
        keys.push(format!("effects.grain.{f}"));
    }
    for f in ["amount", "midpoint", "roundness", "feather", "highlight_contrast", "style"] {
        keys.push(format!("effects.vignette.{f}"));
    }
    for f in ["shadow_tint", "red_hue", "red_saturation", "green_hue", "green_saturation", "blue_hue", "blue_saturation"] {
        keys.push(format!("calibration.{f}"));
    }
    keys.push("look.amount".into());
    for f in [
        "calibration", "colour_adjustments", "detail", "effects", "grayscale_mix", "lens_corrections",
        "mask_group_based_corrections", "red_eye", "retouch", "tone_curve", "transform", "distraction_removal",
    ] {
        keys.push(format!("enable.{f}"));
    }
    keys
}

fn bad() -> DevelopError {
    DevelopError::InvalidPreset
}

fn read(recipe: &DevelopRecipe, key: &str) -> Option<PresetValue> {
    let mut copy = recipe.clone();
    Some(match slot(&mut copy, key)? {
        Slot::Num(v) => PresetValue::Number(*v),
        Slot::Flag(v) => PresetValue::Flag(*v),
        Slot::Seed(v) => PresetValue::Number(f64::from(*v)),
        Slot::Upright(m) => PresetValue::Text(m.key().to_owned()),
        Slot::Style(s) => PresetValue::Text(s.key().to_owned()),
    })
}

/// Whether `value` has the kind `key` takes (`key` must be one of [`preset_keys`]).
fn kind_matches(key: &str, value: &PresetValue) -> bool {
    let flag = key.starts_with("enable.") || matches!(key, "grayscale.convert" | "optics.profile_enable" | "optics.auto_ca");
    match value {
        PresetValue::Flag(_) => flag,
        PresetValue::Text(t) => match key {
            "geometry.upright" => UprightMode::from_key(t).is_some(),
            "effects.vignette.style" => VignetteStyle::from_key(t).is_some(),
            _ => false,
        },
        PresetValue::Number(v) => match key {
            "effects.grain.seed" => v.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(v),
            "geometry.upright" | "effects.vignette.style" => false,
            _ => !flag && v.is_finite(),
        },
    }
}

impl DevelopPreset {
    /// A preset of `keys` with the values they have in `recipe` (e.g. a group of the current edit).
    pub fn capture(recipe: &DevelopRecipe, keys: &[&str], name: &str) -> Result<Self, DevelopError> {
        let mut settings = BTreeMap::new();
        for key in keys {
            let value = read(recipe, key).ok_or_else(bad)?;
            if settings.insert((*key).to_owned(), value).is_some() {
                return Err(bad());
            }
        }
        let mut metadata = BTreeMap::new();
        metadata.insert("name".to_owned(), name.to_owned());
        Ok(Self { metadata, settings, ..Self::default() })
    }

    /// Apply on top of `base` with `amount` (0..=2, 1 = as authored); returns the new recipe,
    /// validated. `base` is not changed.
    pub fn apply(&self, base: &DevelopRecipe, amount: f64) -> Result<DevelopRecipe, DevelopError> {
        if !amount.is_finite() {
            return Err(DevelopError::NonFinite);
        }
        if !(0.0..=2.0).contains(&amount) {
            return Err(DevelopError::InvalidAdjustment);
        }
        let mut out = base.clone();
        let known = preset_keys();
        for (key, value) in &self.settings {
            if !known.iter().any(|k| k == key) || !kind_matches(key, value) {
                return Err(bad());
            }
            let Some(target) = slot(&mut out, key) else {
                // `look.amount` with no look on the base: nothing to scale.
                continue;
            };
            match (target, value) {
                (Slot::Num(v), PresetValue::Number(p)) => *v += amount * (p - *v),
                (Slot::Seed(v), PresetValue::Number(p)) if amount > 0.0 => *v = *p as u32,
                (Slot::Flag(v), PresetValue::Flag(p)) if amount > 0.0 => *v = *p,
                (Slot::Upright(m), PresetValue::Text(t)) if amount > 0.0 => *m = UprightMode::from_key(t).ok_or_else(bad)?,
                (Slot::Style(s), PresetValue::Text(t)) if amount > 0.0 => *s = VignetteStyle::from_key(t).ok_or_else(bad)?,
                _ => {}
            }
        }
        out.validate()?;
        Ok(out)
    }

    /// `self` layered under `over` (keys of `over` win; metadata of `over` wins).
    pub fn merge(&self, over: &DevelopPreset) -> DevelopPreset {
        let mut out = self.clone();
        out.settings.extend(over.settings.clone());
        out.metadata.extend(over.metadata.clone());
        out.flags.extend(over.flags.clone());
        if over.camera_model_restriction.is_some() {
            out.camera_model_restriction.clone_from(&over.camera_model_restriction);
        }
        out
    }

    /// The `hsk-develop-preset 1` text (deterministic order).
    pub fn to_text(&self) -> String {
        let mut s = String::from("hsk-develop-preset 1\n");
        for (k, v) in &self.metadata {
            s.push_str(&format!("meta {k} {v}\n"));
        }
        if let Some(c) = &self.camera_model_restriction {
            s.push_str(&format!("meta camera_model_restriction {c}\n"));
        }
        for (k, v) in &self.flags {
            s.push_str(&format!("flag {k} {v}\n"));
        }
        for (k, v) in &self.settings {
            match v {
                PresetValue::Number(n) => s.push_str(&format!("set {k} {n:?}\n")),
                PresetValue::Flag(b) => s.push_str(&format!("set {k} {b}\n")),
                PresetValue::Text(t) => s.push_str(&format!("set {k} {t}\n")),
            }
        }
        s
    }

    /// Parse a `hsk-develop-preset 1` document (hostile input).
    pub fn parse(bytes: &[u8]) -> Result<Self, DevelopError> {
        if bytes.len() > MAX_PRESET_BYTES {
            return Err(bad());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| bad())?;
        let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
        if lines.next().map(str::trim) != Some("hsk-develop-preset 1") {
            return Err(bad());
        }
        let known = preset_keys();
        let mut out = Self::default();
        for line in lines {
            if line.len() > MAX_LINE {
                return Err(bad());
            }
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            let (kind, rest) = t.split_once(' ').ok_or_else(bad)?;
            let (key, value) = rest.trim().split_once(' ').ok_or_else(bad)?;
            let value = value.trim();
            match kind {
                "meta" => {
                    if value.chars().count() > MAX_META {
                        return Err(bad());
                    }
                    if key == "camera_model_restriction" {
                        if out.camera_model_restriction.replace(value.to_owned()).is_some() {
                            return Err(bad());
                        }
                    } else if !META_FIELDS.contains(&key) || out.metadata.insert(key.to_owned(), value.to_owned()).is_some() {
                        return Err(bad());
                    }
                }
                "flag" => {
                    let v = match value {
                        "true" => true,
                        "false" => false,
                        _ => return Err(bad()),
                    };
                    if !FLAG_FIELDS.contains(&key) || out.flags.insert(key.to_owned(), v).is_some() {
                        return Err(bad());
                    }
                }
                "set" => {
                    if !known.iter().any(|k| k == key) || out.settings.len() >= MAX_PRESET_SETTINGS {
                        return Err(bad());
                    }
                    let v = match value {
                        "true" => PresetValue::Flag(true),
                        "false" => PresetValue::Flag(false),
                        v if v.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '+' || c == '.') => {
                            PresetValue::Number(v.parse::<f64>().map_err(|_| bad())?)
                        }
                        v => PresetValue::Text(v.to_owned()),
                    };
                    if !kind_matches(key, &v) || out.settings.insert(key.to_owned(), v).is_some() {
                        return Err(bad());
                    }
                }
                _ => return Err(bad()),
            }
        }
        Ok(out)
    }
}
