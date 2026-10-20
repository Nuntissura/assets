//! The session document entry `develop` (lane SMB-HARNESS-DEVELOP-2): the develop panel's edit
//! (exposure + hsk-studio-develop `Adjustments`) saved with the package as `studio/develop.<sha256>`
//! and restored on reopen, with the layer id and the tile-list signature of the pixels it produced.
//!
//! Wire: one JSON object, schema `hsk.studio.harness.develop@1`; every field of `Adjustments` is
//! written (lossless round trip; numbers are finite binary64, which JSON round-trips exactly).
//! Local-correction masks: every develop `MaskSource` (`linear_gradient`, `radial_gradient`,
//! `brush`, `luminance_range`, `colour_range`; lane SMB-HARNESS-DEVELOP-3 added the first three).
//!
//! The decoder treats the bytes as hostile: size cap before parsing, serde_json's nesting limit,
//! exact array lengths, finite numbers only, list and string caps (develop's own `MAX_*` limits),
//! unknown keys refused. The values are then validated by develop itself when the viewer builds the
//! recipe (`DevelopRecipe::validate`), so a decodable but out-of-range edit is develop's typed refusal.
#![deny(clippy::arithmetic_side_effects)]

use super::develop::{DevelopEdit, Refusal};
use hsk_studio_develop::{
    AbsoluteWhiteBalance, Adjustments, Brush, ColourGrading, ColourRange, Correction, Detail, GrayscaleMix, HslBand, LinearGradient,
    LocalStack, LuminanceRange, MAX_COLOUR_SAMPLES, MAX_POINT_COLOURS, Mask, MaskBlend, MaskSource, NoiseReduction, PointColour,
    RadialGradient, Sharpen, Wheel,
};
use hsk_studio_develop::local::MAX_DABS;
use serde_json::{Map, Value, json};

pub const SCHEMA: &str = "hsk.studio.harness.develop@1";
/// Largest entry the decoder parses (room for one full brush of `MAX_DABS` dabs).
pub const MAX_ENTRY_BYTES: usize = 8 << 20;
const MAX_TEXT: usize = 256;

/// A decoded entry.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredDevelop {
    pub layer_id: String,
    /// Signature of the layer's tile list the edit produced; `None` when the shown pixels were not
    /// a develop state when saved.
    pub tiles_signature: Option<String>,
    pub edit: DevelopEdit,
}

fn wheel(w: &Wheel) -> Value {
    json!({"hue": w.hue, "saturation": w.saturation, "luminance": w.luminance})
}

fn stack(s: &LocalStack) -> Value {
    json!({"exposure": s.exposure, "contrast": s.contrast, "highlights": s.highlights, "shadows": s.shadows, "whites": s.whites,
        "blacks": s.blacks, "clarity": s.clarity, "saturation": s.saturation, "temperature": s.temperature, "tint": s.tint})
}

fn mask(m: &Mask) -> Result<Value, Refusal> {
    let source = match &m.source {
        MaskSource::LuminanceRange(r) => json!({"kind": "luminance_range", "lo": r.lo, "hi": r.hi, "feather": r.feather}),
        MaskSource::ColourRange(r) => json!({"kind": "colour_range", "samples": r.samples, "breadth": r.breadth}),
        MaskSource::LinearGradient(g) => json!({"kind": "linear_gradient", "zero": [g.zero.0, g.zero.1], "full": [g.full.0, g.full.1], "flipped": g.flipped}),
        MaskSource::RadialGradient(r) => json!({"kind": "radial_gradient", "left": r.left, "top": r.top, "right": r.right, "bottom": r.bottom,
            "angle": r.angle, "midpoint": r.midpoint, "roundness": r.roundness, "feather": r.feather, "flipped": r.flipped}),
        MaskSource::Brush(b) => json!({"kind": "brush", "dabs": b.dabs.iter().map(|d| [d.0, d.1]).collect::<Vec<_>>(), "radius": b.radius, "flow": b.flow, "feather": b.feather}),
        MaskSource::Model(m) => json!({"kind": "model", "source": m.kind.key(), "subcategory": m.subcategory, "model_version": m.model_version}),
    };
    let blend = match m.blend {
        MaskBlend::Add => "add",
        MaskBlend::Subtract => "subtract",
    };
    Ok(json!({"id": m.id, "active": m.active, "inverted": m.inverted, "value": m.value, "blend": blend, "source": source}))
}

/// Encodes an edit (see the module documentation).
pub fn encode(layer_id: &str, tiles_signature: Option<&str>, edit: &DevelopEdit) -> Result<Vec<u8>, Refusal> {
    let a = &edit.adjust;
    let local = a
        .local
        .iter()
        .map(|c| {
            Ok(json!({"id": c.id, "name": c.name, "active": c.active, "amount": c.amount, "stack": stack(&c.stack),
                "masks": c.masks.iter().map(mask).collect::<Result<Vec<_>, Refusal>>()?}))
        })
        .collect::<Result<Vec<_>, Refusal>>()?;
    let p = &a.parametric;
    let g = &a.grading;
    let d = &a.detail;
    let value = json!({
        "schema": SCHEMA,
        "layer_id": layer_id,
        "tiles_signature": tiles_signature,
        "edit": {
            "exposure_stops": edit.exposure_stops,
            "version": [a.version.0, a.version.1],
            "white_balance": {"temperature": a.white_balance.temperature, "tint": a.white_balance.tint},
            "tone": {"contrast": a.tone.contrast, "highlights": a.tone.highlights, "shadows": a.tone.shadows, "whites": a.tone.whites, "blacks": a.tone.blacks},
            "presence": {"vibrance": a.presence.vibrance, "saturation": a.presence.saturation, "clarity": a.presence.clarity},
            "mixer": a.mixer.bands.iter().map(|b| [b.hue, b.saturation, b.luminance]).collect::<Vec<_>>(),
            "parametric": {"highlights": p.highlights, "lights": p.lights, "darks": p.darks, "shadows": p.shadows,
                "shadow_split": p.shadow_split, "midtone_split": p.midtone_split, "highlight_split": p.highlight_split},
            "working_to_xyz_d50": a.working_to_xyz_d50,
            "luma": a.luma,
            "absolute_white_balance": a.absolute_white_balance.map(|w| json!({"kelvin": w.kelvin, "tint": w.tint})),
            "point_colours": a.point_colours.iter().map(|c| json!({"source": c.source, "hue_shift": c.hue_shift, "saturation": c.saturation,
                "luminance": c.luminance, "hue_range": c.hue_range, "saturation_range": c.saturation_range, "luminance_range": c.luminance_range})).collect::<Vec<_>>(),
            "grayscale": {"convert": a.grayscale.convert, "bands": a.grayscale.bands},
            "grading": {"shadows": wheel(&g.shadows), "midtones": wheel(&g.midtones), "highlights": wheel(&g.highlights), "global": wheel(&g.global),
                "blending": g.blending, "balance": g.balance},
            "detail": {"texture": d.texture, "dehaze": d.dehaze,
                "sharpen": {"amount": d.sharpen.amount, "radius": d.sharpen.radius, "detail": d.sharpen.detail, "masking": d.sharpen.masking},
                "noise": {"luminance": d.noise.luminance, "colour": d.noise.colour}},
            "local": local,
        }
    });
    let mut value = value;
    if let Some(edit_object) = value.get_mut("edit").and_then(Value::as_object_mut) {
        edit_object.extend(super::develop_more::encode_families(a));
    }
    let bytes = serde_json::to_vec(&value).map_err(|e| Refusal { code: "develop_entry_encode", detail: e.to_string() })?;
    if bytes.len() > MAX_ENTRY_BYTES {
        return Err(Refusal { code: "develop_entry_too_large", detail: format!("{} bytes, at most {MAX_ENTRY_BYTES}", bytes.len()) });
    }
    Ok(bytes)
}

// ---- decoding ----------------------------------------------------------------------------------------

fn field(path: &str) -> Refusal {
    Refusal { code: "develop_entry_field", detail: path.to_string() }
}

/// An object with exactly `keys` (no unknown key, none missing).
fn object<'a>(v: &'a Value, path: &str, keys: &[&str]) -> Result<&'a Map<String, Value>, Refusal> {
    let o = v.as_object().ok_or_else(|| field(path))?;
    if o.len() != keys.len() || keys.iter().any(|k| !o.contains_key(*k)) {
        return Err(field(path));
    }
    Ok(o)
}

fn num(o: &Map<String, Value>, key: &str, path: &str) -> Result<f64, Refusal> {
    o.get(key).and_then(Value::as_f64).filter(|v| v.is_finite()).ok_or_else(|| field(&format!("{path}.{key}")))
}

fn boolean(o: &Map<String, Value>, key: &str, path: &str) -> Result<bool, Refusal> {
    o.get(key).and_then(Value::as_bool).ok_or_else(|| field(&format!("{path}.{key}")))
}

fn text(o: &Map<String, Value>, key: &str, path: &str) -> Result<String, Refusal> {
    o.get(key).and_then(Value::as_str).filter(|s| s.len() <= MAX_TEXT).map(str::to_string).ok_or_else(|| field(&format!("{path}.{key}")))
}

fn numbers<const N: usize>(v: Option<&Value>, path: &str) -> Result<[f64; N], Refusal> {
    let a = v.and_then(Value::as_array).filter(|a| a.len() == N).ok_or_else(|| field(path))?;
    let mut out = [0.0; N];
    for (slot, x) in out.iter_mut().zip(a) {
        *slot = x.as_f64().filter(|v| v.is_finite()).ok_or_else(|| field(path))?;
    }
    Ok(out)
}

fn list<'a>(v: Option<&'a Value>, path: &str, max: usize) -> Result<&'a Vec<Value>, Refusal> {
    let a = v.and_then(Value::as_array).ok_or_else(|| field(path))?;
    if a.len() > max {
        return Err(Refusal { code: "develop_entry_limit", detail: format!("{path}: {} entries, at most {max}", a.len()) });
    }
    Ok(a)
}

fn decode_wheel(v: Option<&Value>, path: &str) -> Result<Wheel, Refusal> {
    let o = object(v.ok_or_else(|| field(path))?, path, &["hue", "saturation", "luminance"])?;
    Ok(Wheel { hue: num(o, "hue", path)?, saturation: num(o, "saturation", path)?, luminance: num(o, "luminance", path)? })
}

const STACK_KEYS: [&str; 10] = ["exposure", "contrast", "highlights", "shadows", "whites", "blacks", "clarity", "saturation", "temperature", "tint"];

fn decode_stack(v: Option<&Value>, path: &str) -> Result<LocalStack, Refusal> {
    let o = object(v.ok_or_else(|| field(path))?, path, &STACK_KEYS)?;
    let n = |k: &str| num(o, k, path);
    Ok(LocalStack {
        exposure: n("exposure")?,
        contrast: n("contrast")?,
        highlights: n("highlights")?,
        shadows: n("shadows")?,
        whites: n("whites")?,
        blacks: n("blacks")?,
        clarity: n("clarity")?,
        saturation: n("saturation")?,
        temperature: n("temperature")?,
        tint: n("tint")?,
    })
}

fn decode_mask(v: &Value, path: &str, dabs_left: &mut usize) -> Result<Mask, Refusal> {
    let o = object(v, path, &["id", "active", "inverted", "value", "blend", "source"])?;
    let blend = match o.get("blend").and_then(Value::as_str) {
        Some("add") => MaskBlend::Add,
        Some("subtract") => MaskBlend::Subtract,
        _ => return Err(field(&format!("{path}.blend"))),
    };
    let sp = format!("{path}.source");
    let s = o.get("source").ok_or_else(|| field(&sp))?;
    let source = match s.get("kind").and_then(Value::as_str) {
        Some("luminance_range") => {
            let r = object(s, &sp, &["kind", "lo", "hi", "feather"])?;
            MaskSource::LuminanceRange(LuminanceRange { lo: num(r, "lo", &sp)?, hi: num(r, "hi", &sp)?, feather: num(r, "feather", &sp)? })
        }
        Some("colour_range") => {
            let r = object(s, &sp, &["kind", "samples", "breadth"])?;
            let samples = list(r.get("samples"), &format!("{sp}.samples"), MAX_COLOUR_SAMPLES)?
                .iter()
                .map(|x| numbers::<3>(Some(x), &format!("{sp}.samples")))
                .collect::<Result<Vec<_>, _>>()?;
            MaskSource::ColourRange(ColourRange { samples, breadth: num(r, "breadth", &sp)? })
        }
        Some("linear_gradient") => {
            let g = object(s, &sp, &["kind", "zero", "full", "flipped"])?;
            let [zx, zy] = numbers::<2>(g.get("zero"), &format!("{sp}.zero"))?;
            let [fx, fy] = numbers::<2>(g.get("full"), &format!("{sp}.full"))?;
            MaskSource::LinearGradient(LinearGradient { zero: (zx, zy), full: (fx, fy), flipped: boolean(g, "flipped", &sp)? })
        }
        Some("radial_gradient") => {
            let r = object(s, &sp, &["kind", "left", "top", "right", "bottom", "angle", "midpoint", "roundness", "feather", "flipped"])?;
            let n = |k: &str| num(r, k, &sp);
            MaskSource::RadialGradient(RadialGradient {
                left: n("left")?,
                top: n("top")?,
                right: n("right")?,
                bottom: n("bottom")?,
                angle: n("angle")?,
                midpoint: n("midpoint")?,
                roundness: n("roundness")?,
                feather: n("feather")?,
                flipped: boolean(r, "flipped", &sp)?,
            })
        }
        Some("brush") => {
            let b = object(s, &sp, &["kind", "dabs", "radius", "flow", "feather"])?;
            let raw = list(b.get("dabs"), &format!("{sp}.dabs"), *dabs_left)?;
            *dabs_left = dabs_left.saturating_sub(raw.len());
            let dabs = raw.iter().map(|d| numbers::<2>(Some(d), &format!("{sp}.dabs")).map(|[x, y]| (x, y))).collect::<Result<Vec<_>, _>>()?;
            MaskSource::Brush(Brush { dabs, radius: num(b, "radius", &sp)?, flow: num(b, "flow", &sp)?, feather: num(b, "feather", &sp)? })
        }
        Some("model") => {
            let m = object(s, &sp, &["kind", "source", "subcategory", "model_version"])?;
            let kind = m.get("source").and_then(Value::as_str).and_then(hsk_studio_develop::ModelMaskKind::from_key).ok_or_else(|| field(&format!("{sp}.source")))?;
            let subcategory = match m.get("subcategory") {
                Some(Value::Null) => None,
                Some(v) => Some(v.as_u64().and_then(|x| u32::try_from(x).ok()).ok_or_else(|| field(&format!("{sp}.subcategory")))?),
                None => return Err(field(&format!("{sp}.subcategory"))),
            };
            let model_version = match m.get("model_version") {
                Some(Value::Null) => None,
                _ => Some(text(m, "model_version", &sp)?),
            };
            MaskSource::Model(hsk_studio_develop::ModelMask { kind, subcategory, model_version })
        }
        _ => return Err(field(&format!("{sp}.kind"))),
    };
    Ok(Mask { id: text(o, "id", path)?, active: boolean(o, "active", path)?, inverted: boolean(o, "inverted", path)?, value: num(o, "value", path)?, blend, source })
}

fn decode_correction(v: &Value, path: &str, dabs_left: &mut usize) -> Result<Correction, Refusal> {
    let o = object(v, path, &["id", "name", "active", "amount", "stack", "masks"])?;
    let masks = list(o.get("masks"), &format!("{path}.masks"), hsk_studio_develop::local::MAX_MASKS)?
        .iter()
        .enumerate()
        .map(|(j, m)| decode_mask(m, &format!("{path}.masks[{j}]"), dabs_left))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Correction {
        id: text(o, "id", path)?,
        name: text(o, "name", path)?,
        active: boolean(o, "active", path)?,
        amount: num(o, "amount", path)?,
        masks,
        stack: decode_stack(o.get("stack"), &format!("{path}.stack"))?,
    })
}

const EDIT_KEYS: [&str; 15] = [
    "exposure_stops", "version", "white_balance", "tone", "presence", "mixer", "parametric", "working_to_xyz_d50", "luma",
    "absolute_white_balance", "point_colours", "grayscale", "grading", "detail", "local",
];

fn decode_edit(v: &Value) -> Result<DevelopEdit, Refusal> {
    // The lane SMB-DEVELOP-LENS-PRESETS family objects are optional (older entries lack them).
    let o = v.as_object().ok_or_else(|| field("edit"))?;
    if EDIT_KEYS.iter().any(|k| !o.contains_key(*k)) || o.keys().any(|k| !EDIT_KEYS.contains(&k.as_str()) && !super::develop_more::FAMILY_KEYS.contains(&k.as_str())) {
        return Err(field("edit"));
    }
    let sub = |key: &str, keys: &[&str]| -> Result<&Map<String, Value>, Refusal> { object(o.get(key).ok_or_else(|| field(key))?, key, keys) };
    let version = {
        let a = o.get("version").and_then(Value::as_array).filter(|a| a.len() == 2).ok_or_else(|| field("version"))?;
        let part = |i: usize| a.get(i).and_then(Value::as_u64).and_then(|x| u32::try_from(x).ok()).ok_or_else(|| field("version"));
        (part(0)?, part(1)?)
    };
    let wb = sub("white_balance", &["temperature", "tint"])?;
    let tone = sub("tone", &["contrast", "highlights", "shadows", "whites", "blacks"])?;
    let presence = sub("presence", &["vibrance", "saturation", "clarity"])?;
    let mixer = list(o.get("mixer"), "mixer", 8)?;
    if mixer.len() != 8 {
        return Err(field("mixer"));
    }
    let mut bands = [HslBand::default(); 8];
    for (slot, b) in bands.iter_mut().zip(mixer) {
        let [hue, saturation, luminance] = numbers::<3>(Some(b), "mixer")?;
        *slot = HslBand { hue, saturation, luminance };
    }
    let par = sub("parametric", &["highlights", "lights", "darks", "shadows", "shadow_split", "midtone_split", "highlight_split"])?;
    let absolute = match o.get("absolute_white_balance") {
        Some(Value::Null) => None,
        Some(w) => {
            let w = object(w, "absolute_white_balance", &["kelvin", "tint"])?;
            Some(AbsoluteWhiteBalance { kelvin: num(w, "kelvin", "absolute_white_balance")?, tint: num(w, "tint", "absolute_white_balance")? })
        }
        None => return Err(field("absolute_white_balance")),
    };
    let point_colours = list(o.get("point_colours"), "point_colours", MAX_POINT_COLOURS)?
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let path = format!("point_colours[{i}]");
            let c = object(c, &path, &["source", "hue_shift", "saturation", "luminance", "hue_range", "saturation_range", "luminance_range"])?;
            Ok(PointColour {
                source: numbers::<3>(c.get("source"), &path)?,
                hue_shift: num(c, "hue_shift", &path)?,
                saturation: num(c, "saturation", &path)?,
                luminance: num(c, "luminance", &path)?,
                hue_range: num(c, "hue_range", &path)?,
                saturation_range: num(c, "saturation_range", &path)?,
                luminance_range: num(c, "luminance_range", &path)?,
            })
        })
        .collect::<Result<Vec<_>, Refusal>>()?;
    let gray = sub("grayscale", &["convert", "bands"])?;
    let grading = sub("grading", &["shadows", "midtones", "highlights", "global", "blending", "balance"])?;
    let detail = sub("detail", &["texture", "dehaze", "sharpen", "noise"])?;
    let sharpen = object(detail.get("sharpen").ok_or_else(|| field("detail.sharpen"))?, "detail.sharpen", &["amount", "radius", "detail", "masking"])?;
    let noise = object(detail.get("noise").ok_or_else(|| field("detail.noise"))?, "detail.noise", &["luminance", "colour"])?;
    // Brush dabs are bounded across the whole entry (develop's per-brush MAX_DABS).
    let mut dabs_left = MAX_DABS;
    let local = list(o.get("local"), "local", hsk_studio_develop::local::MAX_CORRECTIONS)?
        .iter()
        .enumerate()
        .map(|(i, c)| decode_correction(c, &format!("local[{i}]"), &mut dabs_left))
        .collect::<Result<Vec<_>, _>>()?;
    let mut adjust = Adjustments {
        version,
        white_balance: hsk_studio_develop::WhiteBalanceShift { temperature: num(wb, "temperature", "white_balance")?, tint: num(wb, "tint", "white_balance")? },
        tone: hsk_studio_develop::BasicTone {
            contrast: num(tone, "contrast", "tone")?,
            highlights: num(tone, "highlights", "tone")?,
            shadows: num(tone, "shadows", "tone")?,
            whites: num(tone, "whites", "tone")?,
            blacks: num(tone, "blacks", "tone")?,
        },
        presence: hsk_studio_develop::Presence {
            vibrance: num(presence, "vibrance", "presence")?,
            saturation: num(presence, "saturation", "presence")?,
            clarity: num(presence, "clarity", "presence")?,
        },
        mixer: hsk_studio_develop::ColourMixer { bands },
        parametric: hsk_studio_develop::ParametricCurve {
            highlights: num(par, "highlights", "parametric")?,
            lights: num(par, "lights", "parametric")?,
            darks: num(par, "darks", "parametric")?,
            shadows: num(par, "shadows", "parametric")?,
            shadow_split: num(par, "shadow_split", "parametric")?,
            midtone_split: num(par, "midtone_split", "parametric")?,
            highlight_split: num(par, "highlight_split", "parametric")?,
        },
        working_to_xyz_d50: numbers::<9>(o.get("working_to_xyz_d50"), "working_to_xyz_d50")?,
        luma: numbers::<3>(o.get("luma"), "luma")?,
        local,
        absolute_white_balance: absolute,
        point_colours,
        grayscale: GrayscaleMix { convert: boolean(gray, "convert", "grayscale")?, bands: numbers::<8>(gray.get("bands"), "grayscale.bands")? },
        grading: ColourGrading {
            shadows: decode_wheel(grading.get("shadows"), "grading.shadows")?,
            midtones: decode_wheel(grading.get("midtones"), "grading.midtones")?,
            highlights: decode_wheel(grading.get("highlights"), "grading.highlights")?,
            global: decode_wheel(grading.get("global"), "grading.global")?,
            blending: num(grading, "blending", "grading")?,
            balance: num(grading, "balance", "grading")?,
        },
        detail: Detail {
            texture: num(detail, "texture", "detail")?,
            dehaze: num(detail, "dehaze", "detail")?,
            sharpen: Sharpen {
                amount: num(sharpen, "amount", "detail.sharpen")?,
                radius: num(sharpen, "radius", "detail.sharpen")?,
                detail: num(sharpen, "detail", "detail.sharpen")?,
                masking: num(sharpen, "masking", "detail.sharpen")?,
            },
            noise: NoiseReduction { luminance: num(noise, "luminance", "detail.noise")?, colour: num(noise, "colour", "detail.noise")? },
        },
        // Lens corrections and geometry (develop ADJUST_VERSION 1.2) are not stored by this entry yet
        // (lane SMB-DEVELOP-LENS-PRESETS harness slice); the panel cannot set them before that.
        optics: hsk_studio_develop::Optics::default(),
        geometry: hsk_studio_develop::Geometry::default(),
        effects: hsk_studio_develop::Effects::default(),
        calibration: hsk_studio_develop::Calibration::default(),
        look: None,
    };
    super::develop_more::decode_families(o, &mut adjust)?;
    Ok(DevelopEdit { exposure_stops: num(o, "exposure_stops", "edit")?, adjust })
}

/// Decodes an entry (hostile input; see the module documentation).
pub fn decode(bytes: &[u8]) -> Result<StoredDevelop, Refusal> {
    if bytes.len() > MAX_ENTRY_BYTES {
        return Err(Refusal { code: "develop_entry_too_large", detail: format!("{} bytes, at most {MAX_ENTRY_BYTES}", bytes.len()) });
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|e| Refusal { code: "develop_entry_json", detail: e.to_string() })?;
    let o = v.as_object().ok_or_else(|| field("$"))?;
    if o.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(Refusal { code: "develop_entry_schema", detail: format!("schema {:?}", o.get("schema")) });
    }
    let o = object(&v, "$", &["schema", "layer_id", "tiles_signature", "edit"])?;
    let tiles_signature = match o.get("tiles_signature") {
        Some(Value::Null) => None,
        Some(Value::String(s)) if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) => Some(s.clone()),
        _ => return Err(field("tiles_signature")),
    };
    Ok(StoredDevelop { layer_id: text(o, "layer_id", "$")?, tiles_signature, edit: decode_edit(o.get("edit").ok_or_else(|| field("edit"))?)? })
}
