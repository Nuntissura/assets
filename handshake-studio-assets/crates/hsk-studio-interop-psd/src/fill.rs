//! PSD fill layers (`SoCo` solid colour, `GdFl` gradient fill, `PtFl` pattern fill) mapped to Folio
//! fill records (`hsk_studio_folio::StudioFillLayer`, lane SMB-PS-FILL-LAYERS; spec STU-RAS-007 layer
//! kind `fill`, STU-RAS-025/160/161). The block bytes stay untouched in the document, so the layer
//! round-trips through the exporter; the record is what the renderer reads.
//!
//! Format facts come from the public Adobe Photoshop File Formats Specification ("Descriptor
//! structure", "Adjustment layer" keys `SoCo`, `GdFl`, `PtFl`: a `u32` version 16 and one
//! descriptor) and were checked on the psd-tools corpus files `layers/solid-color-fill.psd`,
//! `layers/gradient-fill.psd` and `layers/pattern-fill.psd` (test data only, no source read):
//!
//! - `SoCo`: `Clr ` = object `RGBC` with `Rd  `, `Grn `, `Bl  ` doubles `0..=255`.
//! - `GdFl`: `Grad` = object `Grdn` (`GrdF` enum `CstS`, `Intr` smoothness 4096 = classic, `Clrs` list of
//!   `Clrt` { `Clr `, `Type` enum, `Lctn` 0..=4096, `Mdpn` 0..=100 }, `Trns` list of `TrnS` { `Opct` percent,
//!   `Lctn`, `Mdpn` }), `Type` enum (`Lnr `, `Rdl `, `Angl`, `Rflc`, `Dmnd`), `Angl` degrees, `Scl `
//!   percent, `Rvrs`, `Dthr`, `Algn`, `Ofst` = object `Pnt ` { `Hrzn`, `Vrtc` percent }.
//! - `PtFl`: `Ptrn` = object { `Nm  `, `Idnt` pattern unique id }, `Scl ` percent, `Algn`, `phase` =
//!   object `Pnt ` { `Hrzn`, `Vrtc` }.
//!
//! The colour and transparency stop lists of a gradient are merged into one list at the union of
//! their locations (the engine has one list; typed `Transformed` loss when transparency exists).
//! Everything this module does not map is a typed loss, never a silent default: an unknown top-level
//! or gradient field is reported once per block (`fill_descriptor_field_unmapped`), and a value
//! outside its range, an unmapped enumeration, a noise gradient or a non-RGB colour keeps the layer
//! opaque with a fixed reason code. A key Photoshop omits at its default (the corpus files carry no
//! `Rvrs`, `Dthr`, `Algn`, `Scl `, `Ofst` or `phase`) takes the format default of the table above.
//! Hostile input (V4-01/02): the descriptor reader bounds depth, item counts and lengths; list
//! lengths are checked against `MAX_STOPS` before any loop; non-finite numbers are refused.
//! Photoshop display parity of the mapped values is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_folio::{
    ProfileBinding, StudioColor, StudioEffectFill, StudioFill, StudioFillBounds, StudioFillGradientStyle,
    StudioFillLayer, StudioGradientStop,
};

use crate::adjust::{Mapped, Note, Reason};
use crate::descriptor::{self, Object, Value};
use crate::header::{ColorMode, Header};
use crate::layers::{PsdLayer, TaggedBlock};
use crate::limits::Limits;
use crate::loss::LossKind;
use crate::patterns::PatternTable;

/// Colour stops, transparency stops and merged stops of one gradient (equal to Folio's ceiling).
const MAX_STOPS: usize = hsk_studio_folio::MAX_FILL_GRADIENT_STOPS;
/// Location axis of a Photoshop gradient stop.
const LOCATION_MAX: f64 = 4096.0;
const TOP_GRADIENT: &[&str] = &["Grad", "Type", "Angl", "Scl ", "Rvrs", "Dthr", "Algn", "Ofst"];
const GRAD_KEYS: &[&str] = &["Nm  ", "GrdF", "Intr", "Clrs", "Trns", "ShTr", "VctC"];
const TOP_PATTERN: &[&str] = &["Ptrn", "Scl ", "Algn", "phase"];
const TOP_SOLID: &[&str] = &["Clr "];

/// Maps the fill `block` of `layer`; `None` when its key is not a fill key.
pub(crate) fn fill_layer(
    layer: &PsdLayer,
    block: &TaggedBlock,
    header: &Header,
    limits: &Limits,
    patterns: &PatternTable,
) -> Option<Result<Mapped<StudioFillLayer>, Reason>> {
    if !matches!(&block.key, b"SoCo" | b"GdFl" | b"PtFl") {
        return None;
    }
    Some(map(layer, block, header, limits, patterns))
}

fn map(
    layer: &PsdLayer,
    block: &TaggedBlock,
    header: &Header,
    limits: &Limits,
    patterns: &PatternTable,
) -> Result<Mapped<StudioFillLayer>, Reason> {
    if layer.little_endian {
        return Err("fill_layer_little_endian");
    }
    if header.color_mode != ColorMode::Rgb {
        return Err("fill_color_mode_unmapped");
    }
    let root = descriptor::parse_versioned(&block.data, &[16], limits)?;
    let mut notes: Vec<Note> = Vec::new();
    let (known, fill) = match &block.key {
        b"SoCo" => (TOP_SOLID, solid(&root)?),
        b"GdFl" => (TOP_GRADIENT, gradient(&root, &mut notes)?),
        _ => (TOP_PATTERN, pattern(&root, patterns)?),
    };
    if root.items.iter().any(|(k, _)| !known.contains(&k.as_str())) {
        notes.push((LossKind::PreservedOpaque, "fill_descriptor_field_unmapped"));
    }
    let bounds = StudioFillBounds { x: 0, y: 0, width: header.width, height: header.height };
    Ok(Mapped { value: StudioFillLayer { bounds, fill }, notes })
}

// ---- values ------------------------------------------------------------------------------------

fn number(o: &Object, key: &str) -> Result<Option<f64>, Reason> {
    match o.get(key) {
        None => Ok(None),
        Some(v) => v.finite_number().map(Some).ok_or("fill_value_invalid"),
    }
}

/// A numeric item that must carry the unit tag `unit` (a bare double is refused: the unit is part
/// of the format and a wrong one would silently change the value's meaning).
fn united(o: &Object, key: &str, unit: &[u8; 4]) -> Result<Option<f64>, Reason> {
    match o.get(key) {
        None => Ok(None),
        Some(v) => {
            if v.unit() != Some(*unit) {
                return Err("fill_unit_unexpected");
            }
            v.finite_number().map(Some).ok_or("fill_value_invalid")
        }
    }
}

/// A boolean item; absent is the format default (Photoshop omits a key at its default: the
/// psd-tools files `layers/gradient-fill.psd`, `layers/pattern-fill.psd` carry no `Rvrs`, `Dthr`,
/// `Algn`, `Scl `, `Ofst` or `phase`), a non-boolean is refused.
fn flag(o: &Object, key: &str, default: bool) -> Result<bool, Reason> {
    match o.get(key) {
        None => Ok(default),
        Some(v) => v.boolean().ok_or("fill_value_invalid"),
    }
}

fn unit_byte(v: f64) -> Result<f64, Reason> {
    if (0.0..=255.0).contains(&v) { Ok(v / 255.0) } else { Err("fill_colour_range") }
}

/// An `RGBC` colour object as a Folio colour in the document's colour space (unassigned binding).
fn colour(v: &Value) -> Result<StudioColor, Reason> {
    let o = v.object().ok_or("fill_colour_missing")?;
    if o.class_id != "RGBC" {
        return Err("fill_colour_model_unmapped");
    }
    let c = |k: &str| number(o, k)?.ok_or("fill_colour_missing").and_then(unit_byte);
    Ok(StudioColor { r: c("Rd  ")?, g: c("Grn ")?, b: c("Bl  ")?, colour_profile_id: ProfileBinding::Unassigned(()) })
}

fn solid(root: &Object) -> Result<StudioFill, Reason> {
    Ok(StudioFill::Solid { colour: colour(root.get("Clr ").ok_or("fill_colour_missing")?)? })
}

// ---- gradient ----------------------------------------------------------------------------------

struct ColourStop {
    at: f64,
    rgb: [f64; 3],
    midpoint: f64,
}
struct OpacityStop {
    at: f64,
    opacity: f64,
    midpoint: f64,
}

fn location(o: &Object) -> Result<f64, Reason> {
    let v = number(o, "Lctn")?.ok_or("gradient_fill_stop_missing")?;
    if (0.0..=LOCATION_MAX).contains(&v) { Ok(v) } else { Err("gradient_fill_range") }
}
fn midpoint(o: &Object) -> Result<f64, Reason> {
    let v = number(o, "Mdpn")?.unwrap_or(50.0);
    if (0.0..=100.0).contains(&v) { Ok(v) } else { Err("gradient_fill_range") }
}

fn list_of<'a>(o: &'a Object, key: &str) -> Result<&'a [Value], Reason> {
    let list = o.get(key).and_then(Value::list).ok_or("gradient_fill_stop_missing")?;
    if list.len() > MAX_STOPS {
        return Err("gradient_fill_stop_count");
    }
    Ok(list)
}

fn interpolate(stops: &[(f64, f64)], at: f64) -> f64 {
    match (stops.first(), stops.last()) {
        (Some(first), _) if at <= first.0 => first.1,
        (_, Some(last)) if at >= last.0 => last.1,
        _ => {
            for w in stops.windows(2) {
                if let [a, b] = w
                    && at >= a.0
                    && at <= b.0
                {
                    let span = b.0 - a.0;
                    return if span > 0.0 { a.1 + (b.1 - a.1) * ((at - a.0) / span) } else { b.1 };
                }
            }
            stops.last().map_or(0.0, |s| s.1)
        }
    }
}

fn stops(grad: &Object, notes: &mut Vec<Note>) -> Result<Vec<StudioGradientStop>, Reason> {
    match grad.get("GrdF").and_then(Value::enum_value) {
        Some("CstS") => {}
        Some("ClNs") => return Err("gradient_fill_noise_unmapped"),
        _ => return Err("gradient_fill_form_unmapped"),
    }
    if grad.items.iter().any(|(k, _)| !GRAD_KEYS.contains(&k.as_str())) {
        notes.push((LossKind::PreservedOpaque, "fill_descriptor_field_unmapped"));
    }
    if number(grad, "Intr")?.is_some_and(|v| v != LOCATION_MAX) {
        notes.push((LossKind::Transformed, "gradient_fill_smoothness_as_linear"));
    }
    let mut colours: Vec<ColourStop> = Vec::new();
    let mut stored_colour_type = false;
    for item in list_of(grad, "Clrs")? {
        let o = item.object().ok_or("gradient_fill_stop_missing")?;
        let rgb = colour(o.get("Clr ").ok_or("fill_colour_missing")?)?;
        if o.get("Type").and_then(Value::enum_value) != Some("UsrS") {
            stored_colour_type = true;
        }
        colours.push(ColourStop { at: location(o)?, rgb: [rgb.r, rgb.g, rgb.b], midpoint: midpoint(o)? });
    }
    if colours.len() < 2 {
        return Err("gradient_fill_stop_count");
    }
    if stored_colour_type {
        // The stop defers to the live foreground or background swatch; the stored colour is mapped.
        notes.push((LossKind::Transformed, "gradient_fill_foreground_background_stop_as_stored"));
    }
    let mut opacities: Vec<OpacityStop> = Vec::new();
    if grad.get("Trns").is_some() {
        for item in list_of(grad, "Trns")? {
            let o = item.object().ok_or("gradient_fill_stop_missing")?;
            let opacity = united(o, "Opct", b"#Prc")?.ok_or("gradient_fill_stop_missing")?;
            if !(0.0..=100.0).contains(&opacity) {
                return Err("gradient_fill_range");
            }
            opacities.push(OpacityStop { at: location(o)?, opacity, midpoint: midpoint(o)? });
        }
    }
    let ordered = |a: &[f64]| a.windows(2).all(|w| matches!(w, [p, q] if p <= q));
    if !ordered(&colours.iter().map(|s| s.at).collect::<Vec<_>>())
        || !ordered(&opacities.iter().map(|s| s.at).collect::<Vec<_>>())
    {
        return Err("gradient_fill_order");
    }
    let position = |at: f64| at / LOCATION_MAX * 100.0;
    let straight = |mid: f64| -> Result<Option<f64>, Reason> {
        if (13.0..=87.0).contains(&mid) { Ok(Some(mid)) } else { Err("gradient_fill_midpoint_range") }
    };
    let translucent = opacities.iter().any(|s| s.opacity != 100.0);
    if !translucent {
        let mut out = Vec::with_capacity(colours.len());
        for s in &colours {
            out.push(StudioGradientStop {
                position: position(s.at),
                midpoint: straight(s.midpoint)?,
                opacity: 100.0,
                colour: StudioColor { r: s.rgb[0], g: s.rgb[1], b: s.rgb[2], colour_profile_id: ProfileBinding::Unassigned(()) },
            });
        }
        return Ok(out);
    }
    // Real transparency: one list at the union of both lists' locations; the engine has one list.
    notes.push((LossKind::Transformed, "gradient_fill_transparency_merged"));
    if colours.iter().any(|s| s.midpoint != 50.0) || opacities.iter().any(|s| s.midpoint != 50.0) {
        notes.push((LossKind::Transformed, "gradient_fill_midpoints_flattened"));
    }
    let mut at: Vec<f64> = colours.iter().map(|s| s.at).chain(opacities.iter().map(|s| s.at)).collect();
    at.sort_by(f64::total_cmp);
    at.dedup();
    if at.len() > MAX_STOPS {
        return Err("gradient_fill_stop_count");
    }
    let channel = |i: usize| -> Vec<(f64, f64)> { colours.iter().map(|s| (s.at, s.rgb[i])).collect() };
    let (r, g, b) = (channel(0), channel(1), channel(2));
    let alpha: Vec<(f64, f64)> = opacities.iter().map(|s| (s.at, s.opacity)).collect();
    Ok(at
        .into_iter()
        .map(|a| StudioGradientStop {
            position: position(a),
            midpoint: Some(50.0),
            opacity: interpolate(&alpha, a),
            colour: StudioColor {
                r: interpolate(&r, a),
                g: interpolate(&g, a),
                b: interpolate(&b, a),
                colour_profile_id: ProfileBinding::Unassigned(()),
            },
        })
        .collect())
}

fn style(root: &Object) -> Result<StudioFillGradientStyle, Reason> {
    match root.get("Type").and_then(Value::enum_value) {
        Some("Lnr ") => Ok(StudioFillGradientStyle::Linear),
        Some("Rdl ") => Ok(StudioFillGradientStyle::Radial),
        Some("Angl") => Ok(StudioFillGradientStyle::Angle),
        Some("Rflc") => Ok(StudioFillGradientStyle::Reflected),
        Some("Dmnd") => Ok(StudioFillGradientStyle::Diamond),
        Some(_) => Err("gradient_fill_style_unmapped"),
        None => Err("gradient_fill_style_missing"),
    }
}

/// A `Pnt ` object `{ Hrzn, Vrtc }` as two numbers; each may carry any unit (the pattern phase is
/// pixels, the gradient offset percent).
fn point(o: &Object, key: &str) -> Result<Option<(f64, f64)>, Reason> {
    let Some(v) = o.get(key) else {
        return Ok(None);
    };
    let p = v.object().ok_or("fill_value_invalid")?;
    let n = |k: &str| number(p, k)?.ok_or("fill_value_invalid");
    Ok(Some((n("Hrzn")?, n("Vrtc")?)))
}

fn gradient(root: &Object, notes: &mut Vec<Note>) -> Result<StudioFill, Reason> {
    let grad = root.get("Grad").and_then(Value::object).ok_or("gradient_fill_gradient_missing")?;
    let stops = stops(grad, notes)?;
    let style = style(root)?;
    let angle = match united(root, "Angl", b"#Ang")? {
        Some(a) if (-180.0..=180.0).contains(&a) => a,
        Some(_) => return Err("gradient_fill_range"),
        None => 0.0,
    };
    let scale = match united(root, "Scl ", b"#Prc")? {
        Some(s) if s > 0.0 => s,
        Some(_) => return Err("gradient_fill_range"),
        None => 100.0,
    };
    let (offset_x, offset_y) = point(root, "Ofst")?.unwrap_or((0.0, 0.0));
    let reverse = flag(root, "Rvrs", false)?;
    let dither = flag(root, "Dthr", false)?;
    let align_with_layer = flag(root, "Algn", true)?;
    Ok(StudioFill::Gradient { style, stops, angle, scale, reverse, dither, align_with_layer, offset_x, offset_y })
}

// ---- pattern -----------------------------------------------------------------------------------

fn pattern(root: &Object, patterns: &PatternTable) -> Result<StudioFill, Reason> {
    let reference = root.get("Ptrn").and_then(Value::object).ok_or("pattern_fill_reference_missing")?;
    let id = match reference.get("Idnt") {
        Some(Value::Text(t)) if !t.trim_end_matches('\0').is_empty() => t.trim_end_matches('\0'),
        _ => return Err("pattern_fill_reference_missing"),
    };
    let scale = match united(root, "Scl ", b"#Prc")? {
        Some(s) if s > 0.0 => s,
        Some(_) => return Err("pattern_fill_range"),
        None => 100.0,
    };
    let (phase_x, phase_y) = point(root, "phase")?.unwrap_or((0.0, 0.0));
    let link_with_layer = flag(root, "Algn", true)?;
    let resolved = patterns.lookup(id).map_err(|code| match code {
        "pattern_overlay_pattern_missing" => "pattern_fill_pattern_missing",
        "pattern_overlay_unminted" => "pattern_fill_pattern_unminted",
        _ => "pattern_fill_pattern_unresolved",
    })?;
    Ok(StudioFill::Pattern {
        pattern: StudioEffectFill {
            schema_id: hsk_studio_folio::PATTERN_SCHEMA.to_owned(),
            object_id: resolved.object_id.clone(),
        },
        pattern_tile: Some(resolved.tile.clone()),
        scale,
        phase_x,
        phase_y,
        link_with_layer,
    })
}
