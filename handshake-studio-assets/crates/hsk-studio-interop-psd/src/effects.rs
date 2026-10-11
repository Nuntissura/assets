//! `lfx2` / `lmfx` layer-effect descriptors mapped to a Folio `StudioEffectStack` record
//! (STU-RAS-040, 156-158). RECORDS only: nothing here renders an effect, so the importer keeps the
//! `layer_effects_not_rendered` loss entry. The descriptor shape (object-effects version u32 = 0,
//! descriptor version u32 = 16, then a descriptor) follows the Adobe Photoshop File Formats
//! Specification; the item keys follow ag-psd `descriptor.ts` `parseEffects` / `parseEffectObject`
//! (read-only reference, MIT): `Scl `, `masterFXSwitch`, the single slots `DrSh`, `IrSh`, `OrGl`,
//! `IrGl`, `ebbl`, `SoFi`, `GrFl`, `patternFill`, `ChFX`, `FrFX`, the multi lists `dropShadowMulti`,
//! `innerShadowMulti`, `solidFillMulti`, `gradientFillMulti`, `frameFXMulti`, and per effect `enab`,
//! `Md  `, `Opct`, `Clr `, `uglg`, `lagl`, `Dstn`, `Ckmt`, `blur`, `Nose`, `AntA`, `TrnS`.
//!
//! Single slots and multi lists are concatenated in descriptor order into one list (STU-RAS-156);
//! a file that carries the same effect in both a single slot and a multi list yields both
//! instances (loss entry `effect_single_and_multi_slots_merged`; whether Photoshop duplicates the
//! first multi entry into the single slot is UNVERIFIED, D5). An effect this module cannot map
//! (gradient or pattern content, an unmapped blend mode, an unknown enumeration, a value outside
//! the Folio range) is skipped with a typed loss entry, never defaulted; the block bytes stay in
//! the document. Blend-mode ids are quoted from ag-psd; the enumeration ids `bvlS`, `bvlD`, `bvlT`,
//! `GlwT`, `glwS`, `Styl`, `PntT` and the bool keys `layerConceals`, `Invr`, `overprint`,
//! `showInDialog` are UNVERIFIED spellings: a file that spells them differently leaves the effect
//! unmapped with a typed loss entry (or the documented default for a bool key).
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_folio::{
    BevelDirection, BevelStyle, BevelTechnique, GlowSource, GlowTechnique, MAX_CURVE_POINTS, MAX_EFFECTS, ProfileBinding,
    StrokeFillType, StrokePosition, StudioColor, StudioContour, StudioContourPoint, StudioEffect, StudioEffectKind,
    StudioEffectStack,
};

use crate::adjust::{Mapped, Note, Reason};
use crate::blend::studio_blend_for_key;
use crate::descriptor::{self, Object, Value};
use crate::layers::PsdLayer;
use crate::limits::Limits;
use crate::loss::LossKind;

/// Blend-mode ids (`BlnM`) quoted from ag-psd `descriptor.ts`, with the PSD layer key each one is.
const BLEND_IDS: [(&str, [u8; 4]); 16] = [
    ("Nrml", *b"norm"),
    ("Mltp", *b"mul "),
    ("Scrn", *b"scrn"),
    ("Ovrl", *b"over"),
    ("SftL", *b"sLit"),
    ("HrdL", *b"hLit"),
    ("CDdg", *b"div "),
    ("CBrn", *b"idiv"),
    ("Drkn", *b"dark"),
    ("Lghn", *b"lite"),
    ("Dfrn", *b"diff"),
    ("Xclu", *b"smud"),
    ("H   ", *b"hue "),
    ("Strt", *b"sat "),
    ("Clr ", *b"colr"),
    ("Lmns", *b"lum "),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    DropShadow,
    InnerShadow,
    OuterGlow,
    InnerGlow,
    Bevel,
    ColourOverlay,
    GradientOverlay,
    PatternOverlay,
    Satin,
    Stroke,
}

fn slot_of(key: &str) -> Option<(Slot, bool)> {
    Some(match key {
        "DrSh" => (Slot::DropShadow, false),
        "dropShadowMulti" => (Slot::DropShadow, true),
        "IrSh" => (Slot::InnerShadow, false),
        "innerShadowMulti" => (Slot::InnerShadow, true),
        "OrGl" => (Slot::OuterGlow, false),
        "IrGl" => (Slot::InnerGlow, false),
        "ebbl" => (Slot::Bevel, false),
        "SoFi" => (Slot::ColourOverlay, false),
        "solidFillMulti" => (Slot::ColourOverlay, true),
        "GrFl" => (Slot::GradientOverlay, false),
        "gradientFillMulti" => (Slot::GradientOverlay, true),
        "patternFill" => (Slot::PatternOverlay, false),
        "ChFX" => (Slot::Satin, false),
        "FrFX" => (Slot::Stroke, false),
        "frameFXMulti" => (Slot::Stroke, true),
        _ => return None,
    })
}

fn has_percent_unit(value: &Value) -> bool {
    value.unit() == Some(*b"#Prc")
}

/// A percent value in `0..=100` (the Folio range of opacity, noise, spread, choke, range, jitter).
fn percent(o: &Object, key: &str, default: f64) -> Result<f64, Reason> {
    match o.get(key) {
        None => Ok(default),
        Some(v) if has_percent_unit(v) => {
            v.number().filter(|n| n.is_finite() && (0.0..=100.0).contains(n)).ok_or("effect_value_invalid")
        }
        Some(_) => Err("effect_unit_unexpected"),
    }
}

/// A percent value with no upper bound (bevel depth).
fn percent_unbounded(o: &Object, key: &str, default: f64) -> Result<f64, Reason> {
    match o.get(key) {
        None => Ok(default),
        Some(v) if has_percent_unit(v) => v.number().filter(|n| n.is_finite()).ok_or("effect_value_invalid"),
        Some(_) => Err("effect_unit_unexpected"),
    }
}

fn pixels(o: &Object, key: &str, default: f64) -> Result<f64, Reason> {
    match o.get(key) {
        None => Ok(default),
        Some(v) if v.unit() == Some(*b"#Pxl") => v.number().filter(|n| n.is_finite() && *n >= 0.0).ok_or("effect_value_invalid"),
        Some(_) => Err("effect_unit_unexpected"),
    }
}

/// An angle in degrees within `lo..=hi` (`-180..=180` for the light angle, `0..=90` for altitude).
fn angle(o: &Object, key: &str, default: f64, lo: f64, hi: f64) -> Result<f64, Reason> {
    match o.get(key) {
        None => Ok(default),
        Some(v) if v.unit() == Some(*b"#Ang") => {
            v.number().filter(|n| n.is_finite() && (lo..=hi).contains(n)).ok_or("effect_value_invalid")
        }
        Some(_) => Err("effect_unit_unexpected"),
    }
}

fn flag(o: &Object, key: &str, default: bool) -> bool {
    o.get(key).and_then(Value::boolean).unwrap_or(default)
}

fn blend(o: &Object, key: &str) -> Result<u16, Reason> {
    let id = o.get(key).and_then(Value::enum_value).ok_or("effect_blend_mode_missing")?;
    BLEND_IDS
        .iter()
        .find(|(name, _)| *name == id)
        .and_then(|(_, psd_key)| studio_blend_for_key(*psd_key))
        .map(|mode| mode.discriminant())
        .ok_or("effect_blend_mode_unmapped")
}

/// RGB colour object (`RGBC`: `Rd  `, `Grn `, `Bl  ` doubles `0..=255`); other colour models are not
/// mapped. The profile stays unassigned: Photoshop stores effect colours in the document's encoded
/// space, which Folio cannot name without inventing a binding.
fn colour(o: &Object, key: &str) -> Result<Option<StudioColor>, Reason> {
    let Some(value) = o.get(key) else {
        return Ok(None);
    };
    let c = value.object().ok_or("effect_colour_model_unmapped")?;
    let channel = |name: &str| -> Result<f64, Reason> {
        let v = c.get(name).and_then(Value::number).ok_or("effect_colour_model_unmapped")?;
        if v.is_finite() && (0.0..=255.0).contains(&v) { Ok(v / 255.0) } else { Err("effect_colour_range") }
    };
    Ok(Some(StudioColor { r: channel("Rd  ")?, g: channel("Grn ")?, b: channel("Bl  ")?, colour_profile_id: ProfileBinding::Unassigned(()) }))
}

fn contour(o: &Object, key: &str) -> Result<Option<StudioContour>, Reason> {
    let Some(value) = o.get(key) else {
        return Ok(None);
    };
    let c = value.object().ok_or("effect_contour_unmapped")?;
    let name = match c.get("Nm  ") {
        Some(Value::Text(t)) if !t.is_empty() && t.len() <= 128 && !t.chars().any(char::is_control) => t.clone(),
        _ => return Err("effect_contour_unmapped"),
    };
    let list = c.get("Crv ").and_then(Value::list).ok_or("effect_contour_unmapped")?;
    if !(2..=MAX_CURVE_POINTS).contains(&list.len()) {
        return Err("effect_contour_unmapped");
    }
    let mut points: Vec<StudioContourPoint> = Vec::with_capacity(list.len());
    for item in list {
        let p = item.object().ok_or("effect_contour_unmapped")?;
        let (input, output) = (p.get("Hrzn").and_then(Value::number), p.get("Vrtc").and_then(Value::number));
        let (Some(input), Some(output)) = (input, output) else {
            return Err("effect_contour_unmapped");
        };
        let in_range = |v: f64| v.is_finite() && (0.0..=255.0).contains(&v);
        if !in_range(input) || !in_range(output) || points.last().is_some_and(|last| last.input >= input) {
            return Err("effect_contour_unmapped");
        }
        points.push(StudioContourPoint { input, output, corner: flag(p, "Cnty", false) });
    }
    Ok(Some(StudioContour { name, points }))
}

fn pick<T: Copy>(o: &Object, key: &str, table: &[(&str, T)]) -> Result<T, Reason> {
    let id = o.get(key).and_then(Value::enum_value).ok_or("effect_enumeration_missing")?;
    table.iter().find(|(name, _)| *name == id).map(|(_, v)| *v).ok_or("effect_enumeration_unmapped")
}

const GLOW_TECHNIQUE: [(&str, GlowTechnique); 2] = [("SfBL", GlowTechnique::Softer), ("PrBL", GlowTechnique::Precise)];

fn map_effect(slot: Slot, o: &Object, ordinal: usize, notes: &mut Vec<Note>) -> Result<StudioEffect, Reason> {
    if matches!(slot, Slot::GradientOverlay | Slot::PatternOverlay) {
        // Gradient and pattern content are references to primitives this importer does not mint.
        return Err("effect_fill_reference_unavailable");
    }
    let opacity = percent(o, "Opct", 100.0)?;
    let mut effect = StudioEffect {
        instance_id: String::new(),
        enabled: flag(o, "enab", true),
        visible_in_editor: flag(o, "showInDialog", true),
        blend_mode: blend(o, "Md  ")?,
        opacity,
        colour: colour(o, "Clr ")?,
        contour: None,
        anti_alias: flag(o, "AntA", false),
        noise: percent(o, "Nose", 0.0)?,
        angle: angle(o, "lagl", 0.0, -180.0, 180.0)?,
        use_global_light: flag(o, "uglg", false),
        params: StudioEffectKind::ColourOverlay,
    };
    match contour(o, "TrnS") {
        Ok(c) => effect.contour = c,
        Err(code) => notes.push((LossKind::Unsupported, code)),
    }
    if o.get("Grad").is_some() {
        return Err("effect_fill_reference_unavailable");
    }
    effect.params = match slot {
        Slot::DropShadow => StudioEffectKind::DropShadow {
            distance: pixels(o, "Dstn", 0.0)?,
            spread: percent(o, "Ckmt", 0.0)?,
            size: pixels(o, "blur", 0.0)?,
            layer_knocks_out: flag(o, "layerConceals", true),
        },
        Slot::InnerShadow => StudioEffectKind::InnerShadow {
            distance: pixels(o, "Dstn", 0.0)?,
            choke: percent(o, "Ckmt", 0.0)?,
            size: pixels(o, "blur", 0.0)?,
        },
        Slot::OuterGlow => StudioEffectKind::OuterGlow {
            technique: pick(o, "GlwT", &GLOW_TECHNIQUE)?,
            spread: percent(o, "Ckmt", 0.0)?,
            size: pixels(o, "blur", 0.0)?,
            range: percent(o, "Inpr", 50.0)?,
            jitter: percent(o, "ShdN", 0.0)?,
            gradient: None,
        },
        Slot::InnerGlow => StudioEffectKind::InnerGlow {
            technique: pick(o, "GlwT", &GLOW_TECHNIQUE)?,
            source: pick(o, "glwS", &[("SrcE", GlowSource::Edge), ("SrcC", GlowSource::Centre)])?,
            choke: percent(o, "Ckmt", 0.0)?,
            size: pixels(o, "blur", 0.0)?,
            range: percent(o, "Inpr", 50.0)?,
            jitter: percent(o, "ShdN", 0.0)?,
            gradient: None,
        },
        Slot::Satin => StudioEffectKind::Satin {
            distance: pixels(o, "Dstn", 0.0)?,
            size: pixels(o, "blur", 0.0)?,
            invert: flag(o, "Invr", false),
        },
        Slot::ColourOverlay => StudioEffectKind::ColourOverlay,
        Slot::Stroke => {
            if pick(o, "PntT", &[("SClr", StrokeFillType::Solid), ("GrFl", StrokeFillType::Gradient), ("Ptrn", StrokeFillType::Pattern)])?
                != StrokeFillType::Solid
            {
                return Err("effect_fill_reference_unavailable");
            }
            StudioEffectKind::Stroke {
                size: pixels(o, "Sz  ", 0.0)?,
                position: pick(
                    o,
                    "Styl",
                    &[("InsF", StrokePosition::Inside), ("OutF", StrokePosition::Outside), ("CtrF", StrokePosition::Centre)],
                )?,
                fill_type: StrokeFillType::Solid,
                fill: None,
                overprint: flag(o, "overprint", false),
            }
        }
        Slot::Bevel => {
            if flag(o, "useTexture", false) {
                return Err("effect_fill_reference_unavailable");
            }
            let altitude = angle(o, "Lald", 30.0, 0.0, 90.0)?;
            StudioEffectKind::BevelEmboss {
                style: pick(
                    o,
                    "bvlS",
                    &[
                        ("InrB", BevelStyle::InnerBevel),
                        ("OtrB", BevelStyle::OuterBevel),
                        ("Embs", BevelStyle::Emboss),
                        ("PlEb", BevelStyle::PillowEmboss),
                        ("strokeEmboss", BevelStyle::StrokeEmboss),
                    ],
                )?,
                technique: pick(
                    o,
                    "bvlT",
                    &[("SfBL", BevelTechnique::Smooth), ("PrBL", BevelTechnique::ChiselHard), ("Slmt", BevelTechnique::ChiselSoft)],
                )?,
                direction: pick(o, "bvlD", &[("In  ", BevelDirection::Up), ("Out ", BevelDirection::Down)])?,
                depth: percent_unbounded(o, "srgR", 100.0)?,
                size: pixels(o, "blur", 0.0)?,
                soften: pixels(o, "Sftn", 0.0)?,
                altitude,
                gloss_contour: None,
                highlight_mode: blend(o, "hglM")?,
                highlight_colour: colour(o, "hglC")?.ok_or("effect_colour_model_unmapped")?,
                highlight_opacity: percent(o, "hglO", 100.0)?,
                shadow_mode: blend(o, "sdwM")?,
                shadow_colour: colour(o, "sdwC")?.ok_or("effect_colour_model_unmapped")?,
                shadow_opacity: percent(o, "sdwO", 100.0)?,
                anti_alias_gloss: flag(o, "antialiasGloss", false),
                use_contour: flag(o, "useShape", false),
                use_texture: false,
                texture: None,
            }
        }
        Slot::GradientOverlay | Slot::PatternOverlay => return Err("effect_fill_reference_unavailable"),
    };
    effect.instance_id = format!("{}-{ordinal}", effect.params.key());
    Ok(effect)
}

/// Maps the effects block of `layer`. `None` when it has no `lfx2` / `lmfx` block; `Err` when the
/// descriptor cannot be read or exceeds the instance ceiling. `stack_id` mints the `SEFX` identity
/// and is only called once the descriptor parsed.
pub(crate) fn effect_stack(
    layer: &PsdLayer,
    stack_id: impl FnOnce() -> Option<String>,
    limits: &Limits,
) -> Option<Result<Mapped<StudioEffectStack>, Reason>> {
    let block = layer.block(b"lfx2").or_else(|| layer.block(b"lmfx"))?;
    Some(map_stack(&block.data, stack_id, limits))
}

fn map_stack(data: &[u8], stack_id: impl FnOnce() -> Option<String>, limits: &Limits) -> Result<Mapped<StudioEffectStack>, Reason> {
    let root = descriptor::parse_versioned(data, &[0, 16], limits)?;
    let mut notes: Vec<Note> = Vec::new();
    let enabled = root.get("masterFXSwitch").and_then(Value::boolean).unwrap_or(true);
    let scale = match root.get("Scl ") {
        None => 100.0,
        Some(v) if has_percent_unit(v) => v.number().filter(|n| n.is_finite() && *n > 0.0).ok_or("effect_scale_invalid")?,
        Some(_) => return Err("effect_scale_invalid"),
    };
    let mut effects: Vec<StudioEffect> = Vec::new();
    let (mut singles, mut multis) = ([false; 10], [false; 10]);
    for (key, value) in &root.items {
        let Some((slot, multi)) = slot_of(key) else {
            continue;
        };
        let index = slot as usize;
        if multi {
            multis[index] = true;
        } else {
            singles[index] = true;
        }
        let objects: Vec<&Object> = match (multi, value) {
            (false, Value::Object(o)) => vec![o],
            (true, Value::List(list)) => list.iter().filter_map(Value::object).collect(),
            _ => {
                notes.push((LossKind::Unsupported, "effect_slot_malformed"));
                continue;
            }
        };
        for object in objects {
            if effects.len() >= MAX_EFFECTS {
                return Err("effects_limit");
            }
            match map_effect(slot, object, effects.len(), &mut notes) {
                Ok(effect) => effects.push(effect),
                Err(code) => notes.push((LossKind::Unsupported, code)),
            }
        }
    }
    if singles.iter().zip(&multis).any(|(single, multi)| *single && *multi) {
        notes.push((LossKind::Transformed, "effect_single_and_multi_slots_merged"));
    }
    notes.push((LossKind::Transformed, "effect_colour_profile_unassigned"));
    let effect_stack_id = stack_id().ok_or("effect_stack_id_unavailable")?;
    Ok(Mapped { value: StudioEffectStack { effect_stack_id, enabled, scale, effects }, notes })
}
