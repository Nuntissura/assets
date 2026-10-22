//! Develop panel families of lane SMB-DEVELOP-LENS-PRESETS (feature `viewer`): lens corrections
//! (`Adjustments::optics`), geometry and Upright (`geometry`), effects (`effects`), calibration
//! (`calibration`), creative looks (`look`), develop presets and model-assisted masks. develop's own
//! types only; every value is validated by `hsk-studio-develop` when the viewer builds the recipe.
//!
//! * Controls (stable ids, sections `lens`, `transform`, `effects`, `calibration`, `look`):
//!   `develop.optics.<distortion_scale|vignetting_scale|manual_distortion|manual_vignette>`,
//!   `develop.optics.defringe.<purple|green>_<amount|hue_low|hue_high>`,
//!   `develop.geometry.<vertical|horizontal|rotate|scale|aspect|offset_x|offset_y>`,
//!   `develop.effects.grain.<amount|size|frequency>`,
//!   `develop.effects.vignette.<amount|midpoint|roundness|feather|highlight_contrast>`,
//!   `develop.calibration.<shadow_tint|red_hue|...|blue_saturation>`, `develop.look.amount` (while a
//!   look is set).
//! * Structure steps ([`FamilyOp`], each one undo step through `DevelopOp::Family`): bind a lens
//!   profile (`hsk-lens-profile 1` bytes + focal / aperture / distance), clear it, the two lens
//!   toggles, the Upright mode, Upright guides, grain seed, vignette style, a creative look from
//!   `.cube` bytes, clear the look, apply a develop preset (`hsk-develop-preset 1` bytes, amount).
//!   Model masks (subject / sky / person ...) are `DevelopOp::AddCorrection(MaskSource::Model)`:
//!   develop refuses them `model_unavailable` (no model provider, nothing downloaded), so nothing
//!   is committed.
//! * Entry codec: the five family objects are optional keys of the stored edit (an entry written
//!   before this lane decodes with identity families); hostile input is refused field by field, the
//!   look's table is re-parsed by Prism's `.cube` tokenizer.
#![deny(clippy::arithmetic_side_effects)]

use super::develop::{DevelopControl, Refusal};
use hsk_studio_develop::{
    Adjustments, Calibration, ChannelModel, CreativeLook, Defringe, DevelopPreset, DevelopRecipe, Effects, Geometry, Grain,
    LensBinding, LensModel, LensProfile, LookDomain, Optics, PerspectiveModel, PostCropVignette, ProfileBinding, UprightMode,
    VignetteStyle,
};
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// Optional keys of the stored edit added by this lane.
pub const FAMILY_KEYS: [&str; 5] = ["optics", "geometry", "effects", "calibration", "look"];

const COUNT: (f64, f64) = (-100.0, 100.0);
const POSITIVE: (f64, f64) = (0.0, 100.0);

type Fixed = (&'static str, &'static str, &'static str, (f64, f64), u8);
const FIXED: &[Fixed] = &[
    ("optics.distortion_scale", "Profile distortion", "lens", (0.0, 200.0), 0),
    ("optics.vignetting_scale", "Profile vignetting", "lens", (0.0, 200.0), 0),
    ("optics.manual_distortion", "Distortion", "lens", COUNT, 0),
    ("optics.manual_vignette", "Lens vignetting", "lens", COUNT, 0),
    ("optics.defringe.purple_amount", "Purple amount", "lens", (0.0, 20.0), 0),
    ("optics.defringe.purple_hue_low", "Purple hue low", "lens", POSITIVE, 0),
    ("optics.defringe.purple_hue_high", "Purple hue high", "lens", POSITIVE, 0),
    ("optics.defringe.green_amount", "Green amount", "lens", (0.0, 20.0), 0),
    ("optics.defringe.green_hue_low", "Green hue low", "lens", POSITIVE, 0),
    ("optics.defringe.green_hue_high", "Green hue high", "lens", POSITIVE, 0),
    ("geometry.vertical", "Vertical", "transform", COUNT, 0),
    ("geometry.horizontal", "Horizontal", "transform", COUNT, 0),
    ("geometry.rotate", "Rotate", "transform", (-10.0, 10.0), 1),
    ("geometry.aspect", "Aspect", "transform", COUNT, 0),
    ("geometry.scale", "Scale", "transform", (50.0, 150.0), 0),
    ("geometry.offset_x", "X offset", "transform", COUNT, 1),
    ("geometry.offset_y", "Y offset", "transform", COUNT, 1),
    ("effects.vignette.amount", "Post-crop vignette", "effects", COUNT, 0),
    ("effects.vignette.midpoint", "Vignette midpoint", "effects", POSITIVE, 0),
    ("effects.vignette.roundness", "Vignette roundness", "effects", COUNT, 0),
    ("effects.vignette.feather", "Vignette feather", "effects", POSITIVE, 0),
    ("effects.vignette.highlight_contrast", "Vignette highlights", "effects", POSITIVE, 0),
    ("effects.grain.amount", "Grain amount", "effects", POSITIVE, 0),
    ("effects.grain.size", "Grain size", "effects", POSITIVE, 0),
    ("effects.grain.frequency", "Grain roughness", "effects", POSITIVE, 0),
    ("calibration.shadow_tint", "Shadows tint", "calibration", COUNT, 0),
    ("calibration.red_hue", "Red primary hue", "calibration", COUNT, 0),
    ("calibration.red_saturation", "Red primary saturation", "calibration", COUNT, 0),
    ("calibration.green_hue", "Green primary hue", "calibration", COUNT, 0),
    ("calibration.green_saturation", "Green primary saturation", "calibration", COUNT, 0),
    ("calibration.blue_hue", "Blue primary hue", "calibration", COUNT, 0),
    ("calibration.blue_saturation", "Blue primary saturation", "calibration", COUNT, 0),
];

/// The panel sections of these families: (id, label, help).
pub const SECTIONS: [(&str, &str, &str); 5] = [
    ("lens", "Lens corrections", "Profile distortion / vignetting, manual distortion and vignetting, defringe."),
    ("transform", "Transform", "Manual perspective, rotate, aspect, scale, offsets; Upright modes are buttons."),
    ("effects", "Effects", "Post-crop vignette and grain (seeded)."),
    ("calibration", "Calibration", "Shadows tint and the red, green, blue primary hue / saturation."),
    ("look", "Creative look", "Amount of the bound .cube look."),
];

/// The numeric controls of the families (the look amount only while a look is set).
pub fn family_controls(a: &Adjustments) -> Vec<DevelopControl> {
    let mut out: Vec<DevelopControl> = FIXED
        .iter()
        .map(|(id, label, section, range, p)| DevelopControl { id: format!("develop.{id}"), label: (*label).to_string(), section, panel_range: *range, precision: *p })
        .collect();
    if a.look.is_some() {
        out.push(DevelopControl { id: "develop.look.amount".into(), label: "Look amount".into(), section: "look", panel_range: (0.0, 2.0), precision: 2 });
    }
    out
}

/// Whether `rest` (a control id without `develop.`) belongs to these families.
pub fn is_family_key(rest: &str) -> bool {
    ["optics.", "geometry.", "effects.", "calibration.", "look."].iter().any(|p| rest.starts_with(p))
}

/// The field a family control writes.
pub fn family_slot<'a>(a: &'a mut Adjustments, rest: &str) -> Option<&'a mut f64> {
    Some(match rest {
        "optics.distortion_scale" => &mut a.optics.lens_profile_distortion_scale,
        "optics.vignetting_scale" => &mut a.optics.lens_profile_vignetting_scale,
        "optics.manual_distortion" => &mut a.optics.manual_distortion_amount,
        "optics.manual_vignette" => &mut a.optics.manual_vignette_amount,
        "optics.defringe.purple_amount" => &mut a.optics.defringe.purple_amount,
        "optics.defringe.purple_hue_low" => &mut a.optics.defringe.purple_hue_low,
        "optics.defringe.purple_hue_high" => &mut a.optics.defringe.purple_hue_high,
        "optics.defringe.green_amount" => &mut a.optics.defringe.green_amount,
        "optics.defringe.green_hue_low" => &mut a.optics.defringe.green_hue_low,
        "optics.defringe.green_hue_high" => &mut a.optics.defringe.green_hue_high,
        "geometry.vertical" => &mut a.geometry.perspective_vertical,
        "geometry.horizontal" => &mut a.geometry.perspective_horizontal,
        "geometry.rotate" => &mut a.geometry.perspective_rotate,
        "geometry.aspect" => &mut a.geometry.perspective_aspect,
        "geometry.scale" => &mut a.geometry.perspective_scale,
        "geometry.offset_x" => &mut a.geometry.perspective_offset_x,
        "geometry.offset_y" => &mut a.geometry.perspective_offset_y,
        "effects.vignette.amount" => &mut a.effects.vignette.amount,
        "effects.vignette.midpoint" => &mut a.effects.vignette.midpoint,
        "effects.vignette.roundness" => &mut a.effects.vignette.roundness,
        "effects.vignette.feather" => &mut a.effects.vignette.feather,
        "effects.vignette.highlight_contrast" => &mut a.effects.vignette.highlight_contrast,
        "effects.grain.amount" => &mut a.effects.grain.amount,
        "effects.grain.size" => &mut a.effects.grain.size,
        "effects.grain.frequency" => &mut a.effects.grain.frequency,
        "calibration.shadow_tint" => &mut a.calibration.shadow_tint,
        "calibration.red_hue" => &mut a.calibration.red_hue,
        "calibration.red_saturation" => &mut a.calibration.red_saturation,
        "calibration.green_hue" => &mut a.calibration.green_hue,
        "calibration.green_saturation" => &mut a.calibration.green_saturation,
        "calibration.blue_hue" => &mut a.calibration.blue_hue,
        "calibration.blue_saturation" => &mut a.calibration.blue_saturation,
        "look.amount" => &mut a.look.as_mut()?.amount,
        _ => return None,
    })
}

/// Group enables the families need: (lens corrections, transform, effects, calibration).
pub fn family_enables(a: &Adjustments) -> (bool, bool, bool, bool) {
    (a.optics != Optics::default(), a.geometry != Geometry::default(), a.effects != Effects::default(), a.calibration != Calibration::default())
}

/// Structured state of the families for agents (part of `develop_json`).
pub fn family_json(a: &Adjustments) -> Value {
    json!({
        "lens_profile": a.optics.profile.as_ref().map(|b| json!({"name": b.name, "sha256": hex(&b.sha256), "focal_mm": b.focal_mm, "samples_used": b.samples_used,
            "residual": b.model.residual.map(|r| [r.0, r.1])})),
        "lens_profile_enable": a.optics.lens_profile_enable,
        "auto_lateral_chromatic_aberration": a.optics.auto_lateral_chromatic_aberration,
        "upright": a.geometry.upright_mode.key(),
        "upright_guides": a.geometry.upright_guides.len(),
        "upright_stored": a.geometry.upright_transforms.is_some(),
        "grain_seed": a.effects.grain.seed,
        "vignette_style": a.effects.vignette.style.key(),
        "look": a.look.as_ref().map(|l| json!({"name": l.name, "sha256": hex(&l.lut.sha256), "size_3d": l.lut.size_3d, "domain": l.domain.key()})),
    })
}

/// Structure changes of these families (each one undo step, like a slider change).
#[derive(Clone, Debug, PartialEq)]
pub enum FamilyOp {
    /// Bind a `hsk-lens-profile 1` profile for the shot (focal mm, APEX aperture value, distance m).
    LensProfile { profile: Vec<u8>, focal_mm: f64, aperture_value: Option<f64>, distance_m: Option<f64> },
    ClearLensProfile,
    LensToggles { profile_enable: bool, auto_ca: bool },
    Upright(UprightMode),
    UprightGuides(Vec<[[f64; 2]; 2]>),
    GrainSeed(u32),
    VignetteStyle(VignetteStyle),
    /// A creative look from `.cube` bytes (Prism tokenizer; `invalid_lut` on refusal).
    Look { name: String, cube: Vec<u8>, domain: LookDomain },
    ClearLook,
    /// Apply a `hsk-develop-preset 1` preset with an amount (0..=2). Its `enable.*` keys are not used:
    /// the panel derives group enables from the active families.
    Preset { preset: Vec<u8>, amount: f64 },
}

fn refusal(code: &'static str, detail: impl Into<String>) -> Refusal {
    Refusal { code, detail: detail.into() }
}

fn develop(e: hsk_studio_develop::DevelopError, what: &str) -> Refusal {
    refusal(e.code(), format!("{what}: {e}"))
}

impl FamilyOp {
    pub fn label(&self) -> String {
        match self {
            Self::LensProfile { focal_mm, .. } => format!("lens profile at {focal_mm} mm"),
            Self::ClearLensProfile => "clear lens profile".into(),
            Self::LensToggles { profile_enable, auto_ca } => format!("lens profile {profile_enable} auto CA {auto_ca}"),
            Self::Upright(m) => format!("upright {}", m.key()),
            Self::UprightGuides(g) => format!("{} upright guides", g.len()),
            Self::GrainSeed(s) => format!("grain seed {s}"),
            Self::VignetteStyle(s) => format!("vignette style {}", s.key()),
            Self::Look { name, .. } => format!("look {name}"),
            Self::ClearLook => "clear look".into(),
            Self::Preset { amount, .. } => format!("preset at {amount}"),
        }
    }

    /// Applies the change to the edit's adjustments and exposure (develop validates afterwards).
    pub fn apply(&self, a: &mut Adjustments, exposure_stops: &mut f64) -> Result<(), Refusal> {
        match self {
            Self::LensProfile { profile, focal_mm, aperture_value, distance_m } => {
                let p = LensProfile::parse(profile).map_err(|e| develop(e, "lens profile"))?;
                a.optics.profile = Some(p.bind(*focal_mm, *aperture_value, *distance_m).map_err(|e| develop(e, "lens profile"))?);
            }
            Self::ClearLensProfile => a.optics.profile = None,
            Self::LensToggles { profile_enable, auto_ca } => {
                a.optics.lens_profile_enable = *profile_enable;
                a.optics.auto_lateral_chromatic_aberration = *auto_ca;
            }
            Self::Upright(m) => a.geometry.upright_mode = *m,
            Self::UprightGuides(g) => a.geometry.upright_guides = g.clone(),
            Self::GrainSeed(s) => a.effects.grain.seed = *s,
            Self::VignetteStyle(s) => a.effects.vignette.style = *s,
            Self::Look { name, cube, domain } => {
                if name.len() > 256 {
                    return Err(refusal("develop_entry_field", "look name over 256 bytes"));
                }
                a.look = Some(CreativeLook::from_cube(name, cube, *domain).map_err(|e| develop(e, "look"))?);
            }
            Self::ClearLook => a.look = None,
            Self::Preset { preset, amount } => {
                let p = DevelopPreset::parse(preset).map_err(|e| develop(e, "preset"))?;
                let id = hsk_studio_accord::DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").map_err(|_| refusal("preset", "binding id"))?;
                let mut r = DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], ProfileBinding { profile_id: id, sha256: [0; 32] });
                r.exposure_stops = *exposure_stops;
                r.adjust = a.clone();
                let out = p.apply(&r, *amount).map_err(|e| develop(e, "preset"))?;
                *exposure_stops = out.exposure_stops;
                *a = out.adjust;
            }
        }
        Ok(())
    }
}

// ---- entry codec -----------------------------------------------------------------------------------

fn field(path: &str) -> Refusal {
    refusal("develop_entry_field", path.to_string())
}

fn obj<'a>(v: Option<&'a Value>, path: &str, keys: &[&str]) -> Result<&'a Map<String, Value>, Refusal> {
    let o = v.and_then(Value::as_object).ok_or_else(|| field(path))?;
    if o.len() != keys.len() || keys.iter().any(|k| !o.contains_key(*k)) {
        return Err(field(path));
    }
    Ok(o)
}

fn num(o: &Map<String, Value>, key: &str, path: &str) -> Result<f64, Refusal> {
    o.get(key).and_then(Value::as_f64).filter(|v| v.is_finite()).ok_or_else(|| field(&format!("{path}.{key}")))
}

fn flag(o: &Map<String, Value>, key: &str, path: &str) -> Result<bool, Refusal> {
    o.get(key).and_then(Value::as_bool).ok_or_else(|| field(&format!("{path}.{key}")))
}

fn nums<const N: usize>(v: Option<&Value>, path: &str) -> Result<[f64; N], Refusal> {
    let a = v.and_then(Value::as_array).filter(|a| a.len() == N).ok_or_else(|| field(path))?;
    let mut out = [0.0; N];
    for (slot, x) in out.iter_mut().zip(a) {
        *slot = x.as_f64().filter(|v| v.is_finite()).ok_or_else(|| field(path))?;
    }
    Ok(out)
}

fn opt_nums<const N: usize>(v: Option<&Value>, path: &str) -> Result<Option<[f64; N]>, Refusal> {
    match v {
        Some(Value::Null) => Ok(None),
        Some(x) => nums::<N>(Some(x), path).map(Some),
        None => Err(field(path)),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: Option<&Value>, path: &str) -> Result<[u8; 32], Refusal> {
    let s = s.and_then(Value::as_str).filter(|s| s.len() == 64).ok_or_else(|| field(path))?;
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(s.as_bytes().chunks_exact(2)) {
        *slot = std::str::from_utf8(pair).ok().and_then(|p| u8::from_str_radix(p, 16).ok()).ok_or_else(|| field(path))?;
    }
    Ok(out)
}

fn short_text(o: &Map<String, Value>, key: &str, path: &str) -> Result<String, Refusal> {
    o.get(key).and_then(Value::as_str).filter(|s| s.len() <= 256).map(str::to_string).ok_or_else(|| field(&format!("{path}.{key}")))
}

fn channel(c: &Option<ChannelModel>) -> Value {
    c.map_or(Value::Null, |c| json!([c.scale, c.radial[0], c.radial[1], c.radial[2]]))
}

fn unchannel(v: Option<&Value>, path: &str) -> Result<Option<ChannelModel>, Refusal> {
    Ok(opt_nums::<4>(v, path)?.map(|[s, a, b, c]| ChannelModel { scale: s, radial: [a, b, c] }))
}

fn distance(d: Option<f64>) -> Value {
    match d {
        None => Value::Null,
        Some(v) if v.is_infinite() => json!("inf"),
        Some(v) => json!(v),
    }
}

fn binding(b: &LensBinding) -> Value {
    let p = &b.model.perspective;
    json!({
        "name": b.name, "sha256": hex(&b.sha256), "focal_mm": b.focal_mm, "aperture_value": b.aperture_value,
        "distance_m": distance(b.distance_m), "samples_used": b.samples_used,
        "perspective": [p.focal_x, p.focal_y, p.centre_x, p.centre_y, p.radial[0], p.radial[1], p.radial[2], p.tangential[0], p.tangential[1], p.scale],
        "vignette": b.model.vignette, "ca_green": channel(&b.model.ca_green), "ca_red_green": channel(&b.model.ca_red_green),
        "ca_blue_green": channel(&b.model.ca_blue_green), "residual": b.model.residual.map(|r| [r.0, r.1]),
    })
}

fn unbinding(v: &Value, path: &str) -> Result<LensBinding, Refusal> {
    let o = obj(Some(v), path, &["name", "sha256", "focal_mm", "aperture_value", "distance_m", "samples_used", "perspective", "vignette", "ca_green", "ca_red_green", "ca_blue_green", "residual"])?;
    let [fx, fy, cx, cy, k1, k2, k3, p1, p2, scale] = nums::<10>(o.get("perspective"), &format!("{path}.perspective"))?;
    let distance_m = match o.get("distance_m") {
        Some(Value::Null) => None,
        Some(Value::String(s)) if s == "inf" => Some(f64::INFINITY),
        Some(x) => Some(x.as_f64().filter(|v| v.is_finite()).ok_or_else(|| field(&format!("{path}.distance_m")))?),
        None => return Err(field(&format!("{path}.distance_m"))),
    };
    let aperture_value = match o.get("aperture_value") {
        Some(Value::Null) => None,
        _ => Some(num(o, "aperture_value", path)?),
    };
    Ok(LensBinding {
        name: short_text(o, "name", path)?,
        sha256: unhex(o.get("sha256"), &format!("{path}.sha256"))?,
        focal_mm: num(o, "focal_mm", path)?,
        aperture_value,
        distance_m,
        samples_used: o.get("samples_used").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).ok_or_else(|| field(&format!("{path}.samples_used")))?,
        model: LensModel {
            perspective: PerspectiveModel { focal_x: fx, focal_y: fy, centre_x: cx, centre_y: cy, radial: [k1, k2, k3], tangential: [p1, p2], scale },
            vignette: opt_nums::<3>(o.get("vignette"), &format!("{path}.vignette"))?,
            ca_green: unchannel(o.get("ca_green"), &format!("{path}.ca_green"))?,
            ca_red_green: unchannel(o.get("ca_red_green"), &format!("{path}.ca_red_green"))?,
            ca_blue_green: unchannel(o.get("ca_blue_green"), &format!("{path}.ca_blue_green"))?,
            residual: opt_nums::<2>(o.get("residual"), &format!("{path}.residual"))?.map(|[a, b]| (a, b)),
        },
    })
}

/// The family objects of the stored edit.
pub fn encode_families(a: &Adjustments) -> Map<String, Value> {
    let o = &a.optics;
    let d = &o.defringe;
    let g = &a.geometry;
    let gr = &a.effects.grain;
    let v = &a.effects.vignette;
    let c = &a.calibration;
    let mut m = Map::new();
    m.insert("optics".into(), json!({
        "lens_profile_enable": o.lens_profile_enable, "profile": o.profile.as_ref().map(binding),
        "distortion_scale": o.lens_profile_distortion_scale, "vignetting_scale": o.lens_profile_vignetting_scale,
        "auto_ca": o.auto_lateral_chromatic_aberration, "manual_distortion": o.manual_distortion_amount, "manual_vignette": o.manual_vignette_amount,
        "defringe": [d.purple_amount, d.purple_hue_low, d.purple_hue_high, d.green_amount, d.green_hue_low, d.green_hue_high],
    }));
    m.insert("geometry".into(), json!({
        "sliders": [g.perspective_vertical, g.perspective_horizontal, g.perspective_rotate, g.perspective_scale, g.perspective_aspect, g.perspective_offset_x, g.perspective_offset_y],
        "upright_mode": g.upright_mode.key(), "upright_centre": [g.upright_centre.0, g.upright_centre.1], "upright_focal_length_35mm": g.upright_focal_length_35mm,
        "upright_guides": g.upright_guides, "upright_transforms": g.upright_transforms,
    }));
    m.insert("effects".into(), json!({
        "grain": [gr.amount, gr.size, gr.frequency], "grain_seed": gr.seed,
        "vignette": [v.amount, v.midpoint, v.roundness, v.feather, v.highlight_contrast], "vignette_style": v.style.key(),
        "override_look_vignette": a.effects.override_look_vignette,
    }));
    m.insert("calibration".into(), json!([c.shadow_tint, c.red_hue, c.red_saturation, c.green_hue, c.green_saturation, c.blue_hue, c.blue_saturation]));
    m.insert("look".into(), a.look.as_ref().map_or(Value::Null, |l| json!({
        "name": l.name, "amount": l.amount, "domain": l.domain.key(), "sha256": hex(&l.lut.sha256), "cube": l.lut.write_cube(),
    })));
    m
}

/// Reads the optional family objects of a stored edit into `a` (absent = identity family).
pub fn decode_families(o: &Map<String, Value>, a: &mut Adjustments) -> Result<(), Refusal> {
    if let Some(v) = o.get("optics") {
        let x = obj(Some(v), "optics", &["lens_profile_enable", "profile", "distortion_scale", "vignetting_scale", "auto_ca", "manual_distortion", "manual_vignette", "defringe"])?;
        let [pa, pl, ph, ga, gl, gh] = nums::<6>(x.get("defringe"), "optics.defringe")?;
        a.optics = Optics {
            lens_profile_enable: flag(x, "lens_profile_enable", "optics")?,
            profile: match x.get("profile") {
                Some(Value::Null) => None,
                Some(b) => Some(unbinding(b, "optics.profile")?),
                None => return Err(field("optics.profile")),
            },
            lens_profile_distortion_scale: num(x, "distortion_scale", "optics")?,
            lens_profile_vignetting_scale: num(x, "vignetting_scale", "optics")?,
            auto_lateral_chromatic_aberration: flag(x, "auto_ca", "optics")?,
            manual_distortion_amount: num(x, "manual_distortion", "optics")?,
            manual_vignette_amount: num(x, "manual_vignette", "optics")?,
            defringe: Defringe { purple_amount: pa, purple_hue_low: pl, purple_hue_high: ph, green_amount: ga, green_hue_low: gl, green_hue_high: gh },
        };
    }
    if let Some(v) = o.get("geometry") {
        let x = obj(Some(v), "geometry", &["sliders", "upright_mode", "upright_centre", "upright_focal_length_35mm", "upright_guides", "upright_transforms"])?;
        let [vert, hor, rot, scale, aspect, ox, oy] = nums::<7>(x.get("sliders"), "geometry.sliders")?;
        let mode = x.get("upright_mode").and_then(Value::as_str).and_then(UprightMode::from_key).ok_or_else(|| field("geometry.upright_mode"))?;
        let [cx, cy] = nums::<2>(x.get("upright_centre"), "geometry.upright_centre")?;
        let guides = x.get("upright_guides").and_then(Value::as_array).filter(|g| g.len() <= hsk_studio_develop::MAX_GUIDES).ok_or_else(|| field("geometry.upright_guides"))?;
        let mut upright_guides = Vec::new();
        for s in guides {
            let pair = s.as_array().filter(|p| p.len() == 2).ok_or_else(|| field("geometry.upright_guides"))?;
            upright_guides.push([nums::<2>(pair.first(), "geometry.upright_guides")?, nums::<2>(pair.get(1), "geometry.upright_guides")?]);
        }
        let upright_transforms = match x.get("upright_transforms") {
            Some(Value::Null) => None,
            Some(Value::Array(t)) if t.len() == 6 => {
                let mut out = [[0.0f64; 9]; 6];
                for (slot, m) in out.iter_mut().zip(t) {
                    *slot = nums::<9>(Some(m), "geometry.upright_transforms")?;
                }
                Some(out)
            }
            _ => return Err(field("geometry.upright_transforms")),
        };
        a.geometry = Geometry {
            perspective_vertical: vert,
            perspective_horizontal: hor,
            perspective_rotate: rot,
            perspective_scale: scale,
            perspective_aspect: aspect,
            perspective_offset_x: ox,
            perspective_offset_y: oy,
            upright_mode: mode,
            upright_centre: (cx, cy),
            upright_focal_length_35mm: num(x, "upright_focal_length_35mm", "geometry")?,
            upright_guides,
            upright_transforms,
        };
    }
    if let Some(v) = o.get("effects") {
        let x = obj(Some(v), "effects", &["grain", "grain_seed", "vignette", "vignette_style", "override_look_vignette"])?;
        let [amount, size, frequency] = nums::<3>(x.get("grain"), "effects.grain")?;
        let seed = x.get("grain_seed").and_then(Value::as_u64).and_then(|s| u32::try_from(s).ok()).ok_or_else(|| field("effects.grain_seed"))?;
        let [va, vm, vr, vf, vh] = nums::<5>(x.get("vignette"), "effects.vignette")?;
        let style = x.get("vignette_style").and_then(Value::as_str).and_then(VignetteStyle::from_key).ok_or_else(|| field("effects.vignette_style"))?;
        a.effects = Effects {
            grain: Grain { amount, size, frequency, seed },
            vignette: PostCropVignette { amount: va, midpoint: vm, roundness: vr, feather: vf, highlight_contrast: vh, style },
            override_look_vignette: flag(x, "override_look_vignette", "effects")?,
        };
    }
    if let Some(v) = o.get("calibration") {
        let [shadow_tint, red_hue, red_saturation, green_hue, green_saturation, blue_hue, blue_saturation] = nums::<7>(Some(v), "calibration")?;
        a.calibration = Calibration { shadow_tint, red_hue, red_saturation, green_hue, green_saturation, blue_hue, blue_saturation };
    }
    if let Some(v) = o.get("look") {
        a.look = match v {
            Value::Null => None,
            _ => {
                let x = obj(Some(v), "look", &["name", "amount", "domain", "sha256", "cube"])?;
                let domain = x.get("domain").and_then(Value::as_str).and_then(LookDomain::from_key).ok_or_else(|| field("look.domain"))?;
                let cube = x.get("cube").and_then(Value::as_str).ok_or_else(|| field("look.cube"))?;
                let mut lut = hsk_studio_prism::parse_cube(cube.as_bytes()).map_err(|e| refusal("develop_entry_field", format!("look.cube: {e}")))?;
                lut.sha256 = unhex(x.get("sha256"), "look.sha256")?;
                Some(CreativeLook { name: short_text(x, "name", "look")?, lut: Arc::new(lut), amount: num(x, "amount", "look")?, domain })
            }
        };
    }
    Ok(())
}
