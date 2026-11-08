//! `photo.*`: selection, fill, liquify, info and develop commands that take a typed record.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::develop::{DevelopOp, default_brush, default_linear, default_radial};
use super::super::develop_more::FamilyOp;
use super::super::fill_tools::{FillSettings, FillSource};
use super::super::info::{InfoCommand, SampleScope};
use super::decode::{Fields, invalid};
use super::{CommandRefusal, read_bounded};
use hsk_studio_develop::local::MAX_DABS;
use hsk_studio_develop::retouch::SpotType;
use hsk_studio_develop::{
    Brush, ColourRange, LinearGradient, LookDomain, LuminanceRange, MAX_COLOUR_SAMPLES, MAX_GUIDES, MAX_LENS_PROFILE_BYTES, MAX_PRESET_BYTES, MaskSource, ModelMask,
    ModelMaskKind, RadialGradient, UprightMode, VignetteStyle,
};
use hsk_studio_pigment::diagnostics::{ColourModel, SampleSize};
use hsk_studio_pigment::liquify::{LiquifyBrush, LiquifyTool};
use hsk_studio_pigment::select_tools::magnetic::MagneticOptions;
use hsk_studio_pigment::select_tools::quick::StrokeMode;
use serde_json::{Value, json};

fn liquify_tool(value: &Value, key: &str) -> Result<LiquifyTool, CommandRefusal> {
    let f = Fields::record(value, key, &["kind", "clockwise", "seed", "dx", "dy"])?;
    Ok(match f.text("kind")?.as_str() {
        "forward_warp" => LiquifyTool::ForwardWarp,
        "push_left" => LiquifyTool::PushLeft,
        "twirl" => LiquifyTool::Twirl { clockwise: f.flag_or("clockwise", true)? },
        "pucker" => LiquifyTool::Pucker,
        "bloat" => LiquifyTool::Bloat,
        "reconstruct" => LiquifyTool::Reconstruct,
        "smooth" => LiquifyTool::Smooth,
        "turbulence" => LiquifyTool::Turbulence { seed: if f.has("seed") { f.unsigned("seed")? } else { 0 } },
        "mesh_clone" => LiquifyTool::MeshClone { dx: f.number("dx")?, dy: f.number("dy")? },
        _ => return Err(invalid("kind", "forward_warp|push_left|twirl|pucker|bloat|reconstruct|smooth|turbulence|mesh_clone")),
    })
}

fn liquify_brush(value: &Value, key: &str) -> Result<LiquifyBrush, CommandRefusal> {
    let f = Fields::record(value, key, &["size", "density", "pressure", "rate"])?;
    Ok(LiquifyBrush { size: f.number("size")?, density: f.number("density")?, pressure: f.number("pressure")?, rate: f.number("rate")? })
}

/// Unset fields keep the viewer's fill-tool settings; stops and gradient geometry stay the window's.
fn fill_settings(v: &Viewer, f: Fields<'_>) -> Result<FillSettings, CommandRefusal> {
    let mut s = v.fill_tools_settings().clone();
    s.opacity_pct = f.number_or("opacity_pct", s.opacity_pct)?;
    s.reverse = f.flag_or("reverse", s.reverse)?;
    s.dither = f.flag_or("dither", s.dither)?;
    s.tolerance = f.number_or("tolerance", s.tolerance)?;
    s.contiguous = f.flag_or("contiguous", s.contiguous)?;
    s.anti_alias = f.flag_or("anti_alias", s.anti_alias)?;
    s.sample_merged = f.flag_or("sample_merged", s.sample_merged)?;
    if let Some(pattern) = f.opt_text("pattern")? {
        s.content = FillSource::Pattern(pattern);
    }
    Ok(s)
}

const MODEL: &[(&str, ColourModel)] = &[("rgb", ColourModel::Rgb), ("hsb", ColourModel::Hsb), ("lab", ColourModel::Lab), ("cmyk", ColourModel::Cmyk)];

fn info_command(id: &str, f: Fields<'_>) -> Result<InfoCommand, CommandRefusal> {
    Ok(match id {
        "photo.info.add_sampler" => InfoCommand::AddSampler { x: f.number("x")?, y: f.number("y")? },
        "photo.info.move_sampler" => InfoCommand::MoveSampler { index: f.index("index")?, x: f.number("x")?, y: f.number("y")? },
        "photo.info.remove_sampler" => InfoCommand::RemoveSampler(f.index("index")?),
        "photo.info.set_sampler_model" => InfoCommand::SetSamplerModel { index: f.index("index")?, model: f.token("model", MODEL)? },
        "photo.info.set_sample_size" => InfoCommand::SetSampleSize(f.token("size", &[("point", SampleSize::Point), ("s3", SampleSize::S3), ("s5", SampleSize::S5), ("s11", SampleSize::S11), ("s31", SampleSize::S31), ("s51", SampleSize::S51), ("s101", SampleSize::S101)])?),
        "photo.info.set_sample_scope" => InfoCommand::SetSampleScope(f.token("scope", &[("current_layer", SampleScope::CurrentLayer), ("current_and_below", SampleScope::CurrentAndBelow), ("all_layers", SampleScope::AllLayers)])?),
        "photo.info.set_model" => InfoCommand::SetModel(f.token("model", MODEL)?),
        other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    })
}

fn pair_tuple(p: [f64; 2]) -> (f64, f64) {
    (p[0], p[1])
}

fn limit(key: &str, count: usize, max: usize) -> CommandRefusal {
    CommandRefusal::new("develop_limit", json!({"key": key, "count": count, "at_most": max}))
}

fn finite(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}

fn pair(v: &Value) -> Option<[f64; 2]> {
    match v.as_array()?.as_slice() {
        [x, y] => Some([finite(x)?, finite(y)?]),
        _ => None,
    }
}

/// A byte cap as the file reader takes it.
fn cap(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

/// The list under `key`, its length checked against `max` before any item is decoded.
fn bounded(f: &Fields<'_>, key: &str, expected: &str, max: usize) -> Result<Vec<Value>, CommandRefusal> {
    let Value::Array(items) = f.json(key, expected)? else {
        return Err(invalid(key, expected));
    };
    if items.len() > max {
        return Err(limit(key, items.len(), max));
    }
    Ok(items)
}

fn colour_samples(f: &Fields<'_>, key: &str) -> Result<Vec<[f64; 3]>, CommandRefusal> {
    let expected = "[[r, g, b], ...]";
    let items = bounded(f, key, expected, MAX_COLOUR_SAMPLES)?;
    let sample = |v: &Value| match v.as_array().map(Vec::as_slice) {
        Some([r, g, b]) => Some([finite(r)?, finite(g)?, finite(b)?]),
        _ => None,
    };
    items.iter().map(sample).collect::<Option<Vec<_>>>().ok_or_else(|| invalid(key, expected))
}

fn dabs(f: &Fields<'_>, key: &str) -> Result<Vec<(f64, f64)>, CommandRefusal> {
    let expected = "[[x, y], ...]";
    let items = bounded(f, key, expected, MAX_DABS)?;
    items.iter().map(|v| pair(v).map(pair_tuple)).collect::<Option<Vec<_>>>().ok_or_else(|| invalid(key, expected))
}

fn guides(f: &Fields<'_>) -> Result<Vec<[[f64; 2]; 2]>, CommandRefusal> {
    let expected = "[[[x, y], [x, y]], ...]";
    let items = bounded(f, "guides", expected, MAX_GUIDES)?;
    let guide = |v: &Value| match v.as_array().map(Vec::as_slice) {
        Some([a, b]) => Some([pair(a)?, pair(b)?]),
        _ => None,
    };
    items.iter().map(guide).collect::<Option<Vec<_>>>().ok_or_else(|| invalid("guides", expected))
}

/// Every optional mask field of the `add_correction` and `add_mask` rows.
const MASK_FIELDS: [&str; 18] = ["zero", "full", "flipped", "left", "top", "right", "bottom", "angle", "midpoint", "roundness", "feather", "dabs", "radius", "flow", "lo", "hi", "samples", "breadth"];
const SOURCES: &str = "linear|radial|brush|luminance_range|colour_range|subject|sky|background|landscape_component|object|person|person_component";

/// The mask source a row names: the panel's default (`default_linear`, `default_radial`,
/// `default_brush`; the panel's luminance window 0.6..1 feather 0.2; colour breadth 0.3) with the given
/// fields. A field the source does not have is refused, never dropped.
fn mask_source(f: &Fields<'_>) -> Result<MaskSource, CommandRefusal> {
    let kind = f.text("source")?;
    let own: &[&str] = match kind.as_str() {
        "linear" => &["zero", "full", "flipped"],
        "radial" => &["left", "top", "right", "bottom", "angle", "midpoint", "roundness", "feather", "flipped"],
        "brush" => &["dabs", "radius", "flow", "feather"],
        "luminance_range" => &["lo", "hi", "feather"],
        "colour_range" => &["samples", "breadth"],
        other if ModelMaskKind::from_key(other).is_some() => &[],
        _ => return Err(invalid("source", SOURCES)),
    };
    if let Some(key) = MASK_FIELDS.iter().find(|k| f.has(k) && !own.contains(k)) {
        return Err(invalid(key, &format!("no field of source {kind}")));
    }
    let point_or = |key: &str, default: (f64, f64)| -> Result<(f64, f64), CommandRefusal> { Ok(f.opt_point(key)?.map_or(default, pair_tuple)) };
    Ok(match kind.as_str() {
        "linear" => match default_linear() {
            MaskSource::LinearGradient(g) => MaskSource::LinearGradient(LinearGradient { zero: point_or("zero", g.zero)?, full: point_or("full", g.full)?, flipped: f.flag_or("flipped", g.flipped)? }),
            other => other,
        },
        "radial" => match default_radial() {
            MaskSource::RadialGradient(g) => MaskSource::RadialGradient(RadialGradient {
                left: f.number_or("left", g.left)?,
                top: f.number_or("top", g.top)?,
                right: f.number_or("right", g.right)?,
                bottom: f.number_or("bottom", g.bottom)?,
                angle: f.number_or("angle", g.angle)?,
                midpoint: f.number_or("midpoint", g.midpoint)?,
                roundness: f.number_or("roundness", g.roundness)?,
                feather: f.number_or("feather", g.feather)?,
                flipped: f.flag_or("flipped", g.flipped)?,
            }),
            other => other,
        },
        "brush" => match default_brush() {
            MaskSource::Brush(g) => MaskSource::Brush(Brush {
                dabs: if f.has("dabs") { dabs(f, "dabs")? } else { g.dabs },
                radius: f.number_or("radius", g.radius)?,
                flow: f.number_or("flow", g.flow)?,
                feather: f.number_or("feather", g.feather)?,
            }),
            other => other,
        },
        "luminance_range" => MaskSource::LuminanceRange(LuminanceRange { lo: f.number_or("lo", 0.6)?, hi: f.number_or("hi", 1.0)?, feather: f.number_or("feather", 0.2)? }),
        "colour_range" => MaskSource::ColourRange(ColourRange { samples: colour_samples(f, "samples")?, breadth: f.number_or("breadth", 0.3)? }),
        model => match ModelMaskKind::from_key(model) {
            Some(kind) => MaskSource::Model(ModelMask { kind, subcategory: None, model_version: None }),
            None => return Err(invalid("source", SOURCES)),
        },
    })
}

const UPRIGHT: [(&str, UprightMode); 6] =
    [("off", UprightMode::Off), ("auto", UprightMode::Auto), ("level", UprightMode::Level), ("vertical", UprightMode::Vertical), ("full", UprightMode::Full), ("guided", UprightMode::Guided)];

/// Most controls one `set_many` names (the panel has a few hundred).
const MAX_CHANGES: usize = 4096;

/// `{control: value, ...}`: every value a finite number (or its text), the count bounded first.
fn changes(f: &Fields<'_>) -> Result<Vec<(String, f64)>, CommandRefusal> {
    let expected = "{control: value, ...}";
    let Value::Object(map) = f.json("changes", expected)? else {
        return Err(invalid("changes", expected));
    };
    if map.len() > MAX_CHANGES {
        return Err(limit("changes", map.len(), MAX_CHANGES));
    }
    map.iter()
        .map(|(control, value)| {
            let parsed = match value {
                Value::String(s) => s.parse::<f64>().ok(),
                other => other.as_f64(),
            };
            parsed.filter(|n| n.is_finite()).map(|n| (control.clone(), n)).ok_or_else(|| invalid(&format!("changes.{control}"), "finite number"))
        })
        .collect()
}

/// Every develop operation the Viewer has, by row (lane SMB-DEVELOP-COMMANDS-1 added the mask-source
/// and family rows). File parameters are read under their engine's byte cap before parsing.
fn develop_op(id: &str, f: Fields<'_>) -> Result<DevelopOp, CommandRefusal> {
    let spot_type = |key: &str| f.token(key, &[("heal", SpotType::Heal), ("clone", SpotType::Clone)]);
    Ok(match id {
        "photo.develop.kelvin" => DevelopOp::Kelvin { kelvin: f.number("kelvin")?, tint: f.number_or("tint", 0.0)? },
        "photo.develop.as_shot_white" => DevelopOp::AsShotWhite,
        "photo.develop.add_point_colour" => DevelopOp::AddPointColour { source: f.vector::<3>("source")? },
        "photo.develop.remove_point_colour" => DevelopOp::RemovePointColour(f.index("index")?),
        "photo.develop.add_luminance_range" => DevelopOp::AddLuminanceRange { lo: f.number("lo")?, hi: f.number("hi")?, feather: f.number_or("feather", 0.0)? },
        "photo.develop.remove_correction" => DevelopOp::RemoveCorrection(f.index("index")?),
        "photo.develop.remove_mask" => DevelopOp::RemoveMask { correction: f.index("correction")?, mask: f.index("mask")? },
        "photo.develop.mask_flags" => DevelopOp::MaskFlags { correction: f.index("correction")?, mask: f.index("mask")?, inverted: f.flag_or("inverted", false)?, subtract: f.flag_or("subtract", false)?, flipped: f.flag_or("flipped", false)? },
        "photo.develop.add_brush_dabs" => DevelopOp::AddBrushDabs { correction: f.index("correction")?, mask: f.index("mask")?, dabs: f.points("dabs")?.into_iter().map(pair_tuple).collect() },
        "photo.develop.grayscale" => DevelopOp::Grayscale(f.flag("on")?),
        "photo.develop.add_spot" => DevelopOp::AddSpot {
            spot_type: spot_type("spot_type")?,
            centre: pair_tuple(f.point("centre")?),
            radius: f.number("radius")?,
            source: f.opt_point("source")?.map(pair_tuple),
            feather: f.number_or("feather", 0.0)?,
            opacity: f.number_or("opacity", 100.0)?,
        },
        "photo.develop.move_spot_source" => DevelopOp::MoveSpotSource { spot: f.index("spot")?, source: pair_tuple(f.point("source")?) },
        "photo.develop.set_spot" => DevelopOp::SetSpot { spot: f.index("spot")?, spot_type: spot_type("spot_type")?, feather: f.number("feather")?, opacity: f.number("opacity")?, radius: f.number("radius")? },
        "photo.develop.remove_spot" => DevelopOp::RemoveSpot(f.index("spot")?),
        "photo.develop.add_distraction" => DevelopOp::AddDistraction { centre: pair_tuple(f.point("centre")?), radius: f.number("radius")? },
        "photo.develop.remove_distraction" => DevelopOp::RemoveDistraction(f.index("index")?),
        "photo.develop.add_colour_range" => DevelopOp::AddColourRange { samples: colour_samples(&f, "samples")?, breadth: f.number("breadth")? },
        "photo.develop.add_correction" => DevelopOp::AddCorrection(mask_source(&f)?),
        "photo.develop.add_mask" => DevelopOp::AddMask { correction: f.index("correction")?, source: mask_source(&f)?, subtract: f.flag_or("subtract", false)? },
        "photo.develop.lens_profile" => {
            let profile = read_bounded(&f.path("path")?, cap(MAX_LENS_PROFILE_BYTES))?;
            let distance_m = match f.opt_text("distance_m")?.as_deref() {
                None => None,
                Some("inf") => Some(f64::INFINITY),
                Some(_) => Some(f.number("distance_m")?),
            };
            DevelopOp::Family(FamilyOp::LensProfile { profile, focal_mm: f.number("focal_mm")?, aperture_value: f.opt_number("aperture_value")?, distance_m })
        }
        "photo.develop.clear_lens_profile" => DevelopOp::Family(FamilyOp::ClearLensProfile),
        "photo.develop.lens_toggles" => DevelopOp::Family(FamilyOp::LensToggles { profile_enable: f.flag("profile_enable")?, auto_ca: f.flag("auto_ca")? }),
        "photo.develop.upright" => DevelopOp::Family(FamilyOp::Upright(f.token("mode", &UPRIGHT)?)),
        "photo.develop.upright_guides" => DevelopOp::Family(FamilyOp::UprightGuides(guides(&f)?)),
        "photo.develop.grain_seed" => DevelopOp::Family(FamilyOp::GrainSeed(f.count("seed")?)),
        "photo.develop.vignette_style" => {
            DevelopOp::Family(FamilyOp::VignetteStyle(f.token("style", &[("highlight_priority", VignetteStyle::HighlightPriority), ("colour_priority", VignetteStyle::ColourPriority)])?))
        }
        "photo.develop.look" => {
            let domain = f.token("domain", &[("srgb_encoded", LookDomain::SrgbEncoded), ("linear", LookDomain::Linear)])?;
            let cube = read_bounded(&f.path("path")?, cap(hsk_studio_prism::MAX_CUBE_BYTES))?;
            DevelopOp::Family(FamilyOp::Look { name: f.text("name")?, cube, domain })
        }
        "photo.develop.clear_look" => DevelopOp::Family(FamilyOp::ClearLook),
        "photo.develop.apply_preset" => DevelopOp::Family(FamilyOp::Preset { preset: read_bounded(&f.path("path")?, cap(MAX_PRESET_BYTES))?, amount: f.number("amount")? }),
        other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    })
}

/// `Err(refusal)` before the method ran; the method's own outcome otherwise.
pub(super) fn run(v: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Result<Value, String>, CommandRefusal> {
    if id.starts_with("photo.info.") {
        return Ok(v.info_command(info_command(id, f)?).map(|()| Value::Null));
    }
    if id.starts_with("photo.develop.") {
        let result = match id {
            "photo.develop.set" => v.set_develop(&f.text("control")?, f.number("value")?),
            "photo.develop.set_many" => {
                let changes = changes(&f)?;
                let pairs: Vec<(&str, f64)> = changes.iter().map(|(c, n)| (c.as_str(), *n)).collect();
                v.set_develop_many(&pairs)
            }
            _ => v.develop_op(develop_op(id, f)?),
        };
        return match result {
            Ok(revision) => Ok(Ok(revision.into())),
            Err(r) => Err(CommandRefusal::new(r.code, json!({"message": r.detail}))),
        };
    }
    let layer = || f.text("layer");
    Ok(match id {
        "photo.liquify" => v.liquify(&layer()?, liquify_tool(&f.json("tool", "a liquify tool")?, "tool")?, &f.samples("samples")?, &liquify_brush(&f.json("brush", "a liquify brush")?, "brush")?).map(Into::into),
        "photo.select_quick" => {
            let mode = if f.has("mode") { f.token("mode", &[("add", StrokeMode::Add), ("subtract", StrokeMode::Subtract)])? } else { StrokeMode::Add };
            v.select_quick_merged(&layer()?, &f.samples("samples")?, f.number("diameter_px")?, mode, f.flag_or("sample_merged", false)?).map(|()| Value::Null)
        }
        "photo.select_magnetic" => {
            let base = MagneticOptions::default();
            let options = MagneticOptions {
                width: f.number_or("width", base.width)?,
                contrast: f.number_or("contrast", base.contrast)?,
                frequency: f.number_or("frequency", base.frequency)?,
                sample_merged: f.flag_or("sample_merged", base.sample_merged)?,
                margin_px: if f.has("margin_px") { f.count("margin_px")? } else { base.margin_px },
            };
            v.select_magnetic(&layer()?, &f.points("trace")?, &options).map(|()| Value::Null)
        }
        "photo.refine_stroke" => v.refine_stroke(&layer()?, &f.samples("samples")?).map(|()| Value::Null),
        "photo.flood_fill" => {
            let settings = fill_settings(v, f)?;
            v.flood_fill(&layer()?, f.number("x")?, f.number("y")?, &settings).map(Into::into)
        }
        "photo.fill_selection" => {
            let settings = fill_settings(v, f)?;
            v.fill_selection(&layer()?, &settings).map(Into::into)
        }
        "photo.fill_gradient" => {
            let settings = fill_settings(v, f)?;
            v.fill_gradient(&layer()?, f.point("from")?, f.point("to")?, &settings).map(Into::into)
        }
        other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    })
}
