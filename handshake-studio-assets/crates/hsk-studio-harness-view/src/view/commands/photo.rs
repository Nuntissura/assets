//! `photo.*`: selection, fill, liquify, info and develop commands that take a typed record.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::develop::DevelopOp;
use super::super::fill_tools::{FillSettings, FillSource};
use super::super::info::{InfoCommand, SampleScope};
use super::CommandRefusal;
use super::decode::{Fields, invalid};
use hsk_studio_develop::retouch::SpotType;
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

/// The develop operations with no mask-source or tone-family record: those stay Rust-only.
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
        other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    })
}

/// `Err(refusal)` before the method ran; the method's own outcome otherwise.
pub(super) fn run(v: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Result<Value, String>, CommandRefusal> {
    if id.starts_with("photo.info.") {
        return Ok(v.info_command(info_command(id, f)?).map(|()| Value::Null));
    }
    if id.starts_with("photo.develop.") {
        return match v.develop_op(develop_op(id, f)?) {
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
