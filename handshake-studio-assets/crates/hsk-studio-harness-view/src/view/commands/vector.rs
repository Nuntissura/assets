//! `vector.*`, `proof.*` and `design.*`: the Viewer methods that take a whole typed record. Each row
//! calls the method the window calls; the method names its own undo step.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::proof::ProofCommand;
use super::super::vector::mesh::MeshEdit;
use super::super::vector::path_text::PathTextSpec;
use super::super::vector::swatch::{SwatchColour, SwatchSpec};
use super::super::vector::{LiveTextKind, NetworkEdit};
use super::decode::{Fields, folio_field, invalid, paint};
use super::{CommandRefusal, read_bounded};
use hsk_studio_components::GridTrackMove;
use hsk_studio_folio as folio;
use hsk_studio_prism::{ColorizeMethod, RecolorAdjust, RecolorRow, RecolorTable, Rgb};
use serde_json::{Value, json};

/// A font or profile file the process reads for one command.
const MAX_RESOURCE_BYTES: u64 = 268_435_456;

fn swatch_colour(value: &Value, key: &str) -> Result<SwatchColour, CommandRefusal> {
    let f = Fields::record(value, key, &["model", "components", "gray"])?;
    Ok(match f.text("model")?.as_str() {
        "rgb" => SwatchColour::Rgb(f.vector::<3>("components")?),
        "cmyk" => SwatchColour::Cmyk(f.vector::<4>("components")?),
        "lab" => SwatchColour::Lab(f.vector::<3>("components")?),
        "gray" => SwatchColour::Gray(f.number("gray")?),
        _ => return Err(invalid("model", "rgb|cmyk|lab|gray")),
    })
}

/// `{"kind":"process"|"global","colour":{...}}`, `{"kind":"spot","ink":...,"measured_lab":[l,a,b],
/// "alternate":{...}}`, `{"kind":"tint","base":id,"percent":p}` or `{"kind":"gradient","paint":{...}}`.
fn swatch_spec(value: &Value, key: &str) -> Result<SwatchSpec, CommandRefusal> {
    let f = Fields::record(value, key, &["kind", "colour", "ink", "measured_lab", "alternate", "base", "percent", "paint"])?;
    Ok(match f.text("kind")?.as_str() {
        "process" => SwatchSpec::Process(swatch_colour(&f.json("colour", "a colour")?, "colour")?),
        "global" => SwatchSpec::Global(swatch_colour(&f.json("colour", "a colour")?, "colour")?),
        "spot" => SwatchSpec::Spot {
            ink: f.text("ink")?,
            measured_lab: if f.has("measured_lab") { Some(f.vector::<3>("measured_lab")?) } else { None },
            alternate: if f.has("alternate") { Some(swatch_colour(&f.json("alternate", "a colour")?, "alternate")?) } else { None },
        },
        "tint" => SwatchSpec::Tint { base: f.text("base")?, percent: f.number("percent")? },
        "gradient" => SwatchSpec::Gradient(paint(&f.json("paint", "a paint")?, "paint")?),
        _ => return Err(invalid("kind", "process|global|spot|tint|gradient")),
    })
}

fn network_edit(value: &Value, key: &str) -> Result<NetworkEdit, CommandRefusal> {
    let f = Fields::record(value, key, &["kind", "vertex", "dx", "dy", "x", "y", "a", "b", "segment", "start", "end"])?;
    Ok(match f.text("kind")?.as_str() {
        "move_vertex" => NetworkEdit::MoveVertex { vertex: f.index("vertex")?, dx: f.number("dx")?, dy: f.number("dy")? },
        "add_vertex" => NetworkEdit::AddVertex { x: f.number("x")?, y: f.number("y")? },
        "connect" => NetworkEdit::Connect { a: f.index("a")?, b: f.index("b")? },
        "delete_segment" => NetworkEdit::DeleteSegment(f.index("segment")?),
        "tangents" => NetworkEdit::Tangents { segment: f.index("segment")?, start: f.opt_point("start")?, end: f.opt_point("end")? },
        _ => return Err(invalid("kind", "move_vertex|add_vertex|connect|delete_segment|tangents")),
    })
}

fn mesh_edit(value: &Value, key: &str) -> Result<MeshEdit, CommandRefusal> {
    let f = Fields::record(value, key, &["kind", "row", "column", "rgb", "opacity", "dx", "dy", "at"])?;
    Ok(match f.text("kind")?.as_str() {
        "set_point" => MeshEdit::SetPoint { row: f.count("row")?, column: f.count("column")?, rgb: f.vector::<3>("rgb")?, opacity: f.number_or("opacity", 1.0)? },
        "move_point" => MeshEdit::MovePoint { row: f.count("row")?, column: f.count("column")?, dx: f.number("dx")?, dy: f.number("dy")? },
        "insert_column" => MeshEdit::InsertColumn { column: f.count("column")?, at: f.number("at")? },
        "insert_row" => MeshEdit::InsertRow { row: f.count("row")?, at: f.number("at")? },
        "remove_column" => MeshEdit::RemoveColumn(f.count("column")?),
        "remove_row" => MeshEdit::RemoveRow(f.count("row")?),
        _ => return Err(invalid("kind", "set_point|move_point|insert_column|insert_row|remove_column|remove_row")),
    })
}

fn rgb(value: &Value, key: &str) -> Result<Rgb, CommandRefusal> {
    match value.as_array().map(Vec::as_slice) {
        Some([r, g, b]) => match (r.as_f64(), g.as_f64(), b.as_f64()) {
            (Some(r), Some(g), Some(b)) if r.is_finite() && g.is_finite() && b.is_finite() => Ok([r, g, b]),
            _ => Err(invalid(key, "[r, g, b], finite")),
        },
        _ => Err(invalid(key, "[r, g, b], finite")),
    }
}

/// `{"rows":[{"current":[[r,g,b],...],"new":[r,g,b]}],"method":...,"preserve_white":...}`.
fn recolor_table(value: &Value, key: &str) -> Result<RecolorTable, CommandRefusal> {
    let f = Fields::record(value, key, &["rows", "method", "preserve_white", "preserve_black", "preserve_grays"])?;
    let list = f.json("rows", "a list of rows")?;
    let Some(items) = list.as_array() else { return Err(invalid("rows", "a list of rows")) };
    let rows = items
        .iter()
        .map(|item| {
            let row = Fields::record(item, "rows", &["current", "new"])?;
            let current = row.json("current", "a list of colours")?;
            let Some(colours) = current.as_array() else { return Err(invalid("current", "a list of colours")) };
            Ok(RecolorRow {
                current: colours.iter().map(|c| rgb(c, "current")).collect::<Result<_, _>>()?,
                new: if row.has("new") { Some(rgb(&row.json("new", "a colour")?, "new")?) } else { None },
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let method = if f.has("method") {
        f.token("method", &[("exact", ColorizeMethod::Exact), ("scale_tints", ColorizeMethod::ScaleTints), ("preserve_tints", ColorizeMethod::PreserveTints), ("tints_and_shades", ColorizeMethod::TintsAndShades), ("hue_shift", ColorizeMethod::HueShift)])?
    } else {
        ColorizeMethod::default()
    };
    Ok(RecolorTable { rows, method, preserve_white: f.flag_or("preserve_white", false)?, preserve_black: f.flag_or("preserve_black", false)?, preserve_grays: f.flag_or("preserve_grays", false)? })
}

fn recolor_adjust(value: &Value, key: &str) -> Result<RecolorAdjust, CommandRefusal> {
    let f = Fields::record(value, key, &["hue_shift_deg", "saturation_shift", "brightness_shift", "temperature_shift", "luminosity_shift"])?;
    Ok(RecolorAdjust {
        hue_shift_deg: f.number_or("hue_shift_deg", 0.0)?,
        saturation_shift: f.number_or("saturation_shift", 0.0)?,
        brightness_shift: f.number_or("brightness_shift", 0.0)?,
        temperature_shift: f.number_or("temperature_shift", 0.0)?,
        luminosity_shift: f.number_or("luminosity_shift", 0.0)?,
    })
}

/// Id of a method's outcome as a text id.
fn id_of(result: Result<String, String>) -> Result<Value, String> {
    result.map(Value::String)
}

fn done(result: Result<(), String>) -> Result<Value, String> {
    result.map(|()| Value::Null)
}

/// `Err(refusal)` before the method ran; the method's own outcome otherwise.
pub(super) fn run_vector(v: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Result<Value, String>, CommandRefusal> {
    let layer = || f.text("layer");
    Ok(match id {
        "vector.set_fill" => {
            let spec = if f.has("paint") { Some(paint(&f.json("paint", "a paint")?, "paint")?) } else { None };
            id_of(v.set_fill(&layer()?, spec.as_ref()))
        }
        "vector.set_stroke" => {
            let spec = if f.has("stroke") { Some(super::decode::stroke_panel(&f.json("stroke", "a stroke panel")?, "stroke")?) } else { None };
            id_of(v.set_stroke(&layer()?, spec.as_ref()))
        }
        "vector.set_stroke_markers" => {
            let spec = if f.has("markers") { Some(super::panel::marker_spec(&f.json("markers", "markers")?, "markers")?) } else { None };
            id_of(v.set_stroke_markers(&layer()?, f.count("row")?, spec))
        }
        "vector.add_swatch" => id_of(v.add_swatch(&f.text("name")?, &paint(&f.json("paint", "a paint")?, "paint")?)),
        "vector.edit_swatch" => id_of(v.edit_swatch(&f.text("swatch")?, &paint(&f.json("paint", "a paint")?, "paint")?)),
        "vector.add_folio_swatch" => id_of(v.add_folio_swatch(&f.text("name")?, &swatch_spec(&f.json("spec", "a swatch")?, "spec")?)),
        "vector.edit_folio_swatch" => id_of(v.edit_folio_swatch(&f.text("swatch")?, &swatch_spec(&f.json("spec", "a swatch")?, "spec")?)),
        "vector.edit_path" => id_of(v.edit_path(&layer()?, super::panel::path_edit(&f.json("edit", "a path edit")?, "edit")?)),
        "vector.edit_network" => id_of(v.edit_network(&layer()?, network_edit(&f.json("edit", "a network edit")?, "edit")?)),
        "vector.edit_gradient_mesh" => id_of(v.edit_gradient_mesh(&layer()?, mesh_edit(&f.json("edit", "a mesh edit")?, "edit")?)),
        "vector.set_live_shape" => id_of(v.set_live_shape(&layer()?, folio_field!(f, "params", folio::StudioShapeParameters, "live shape parameters")?)),
        "vector.make_envelope" => {
            id_of(v.make_envelope(&layer()?, &super::panel::envelope_spec(&f.json("spec", "an envelope")?, "spec")?, f.number_or("fidelity", 100.0)?, f.flag_or("distort_appearance", false)?))
        }
        "vector.make_vector_mask" => id_of(v.make_vector_mask(&f.text("target")?, &f.text("source")?, folio_field!(f, "mode", folio::StudioMaskMode, "ALPHA|VECTOR|LUMINANCE")?, f.flag_or("invert", false)?)),
        "vector.align_layers" => done(v.align_layers(&f.list("layers")?, &super::panel::align_target(f)?, super::panel::align(f)?)),
        "vector.distribute_layers" => done(v.distribute_layers(&f.list("layers")?, &super::panel::align_target(f)?, super::panel::distribute(f)?)),
        "vector.transform_layers" => done(v.transform_layers(&f.list("layers")?, super::panel::transform_op(&f.json("op", "a transform")?, "op")?, super::panel::reference(f)?)),
        "vector.transform_each_layers" => done(v.transform_each_layers(&f.list("layers")?, &super::panel::transform_each(&f.json("each", "a transform-each record")?, "each")?)),
        "vector.recolor_artwork" => {
            let table = recolor_table(&f.json("table", "a recolor table")?, "table")?;
            let adjust = if f.has("adjust") { recolor_adjust(&f.json("adjust", "an adjustment")?, "adjust")? } else { RecolorAdjust::default() };
            v.recolor_artwork(&f.list("layers")?, &table, &adjust).map(|report| json!({"report": format!("{report:?}")}))
        }
        "vector.bind_live_text_with_font" => {
            let kind = match f.text("kind")?.as_str() {
                "path" => LiveTextKind::Path,
                "area" => LiveTextKind::Area { inset_pt: f.number_or("inset_pt", 0.0)? },
                _ => return Err(invalid("kind", "path|area")),
            };
            id_of(v.bind_live_text_with_font(&layer()?, &f.text("text")?, kind, &f.text("family")?))
        }
        "vector.text_on_path_to_outlines" => {
            let spec = PathTextSpec { text: f.text("text")?, family: f.text("family")?, size_pt: f.number("size_pt")?, options: hsk_studio_type::PathTextOptions::default() };
            v.text_on_path_to_outlines(&layer()?, &spec).map(|(id, _)| Value::String(id))
        }
        "vector.place_text_with_font" => id_of(v.place_text_with_font(f.number("x")?, f.number("y")?, &f.text("text")?, &f.text("family")?)),
        "vector.add_font" => {
            let bytes = read_bounded(&f.path("path")?, MAX_RESOURCE_BYTES)?;
            v.add_font(&f.text("family")?, bytes);
            Ok(Value::Null)
        }
        "vector.set_document_cmyk_profile" => {
            let bytes = read_bounded(&f.path("path")?, MAX_RESOURCE_BYTES)?;
            id_of(v.set_document_cmyk_profile(&f.text("name")?, &bytes))
        }
        "vector.layer_bounds" => v.layer_bounds(&layer()?).map(|b| json!({"min": b.min, "max": b.max})),
        "vector.set_tool_options" => {
            let mut tool = v.vector_tool().clone();
            if let Some(c) = f.opt_rgb8("fill")? {
                tool.fill = c;
                tool.fill_on = true;
            }
            tool.fill_on = f.flag_or("fill_on", tool.fill_on)?;
            if let Some(c) = f.opt_rgb8("stroke")? {
                tool.stroke = c;
                tool.stroke_on = true;
            }
            tool.stroke_on = f.flag_or("stroke_on", tool.stroke_on)?;
            tool.stroke_width = f.number_or("stroke_width", tool.stroke_width)?;
            tool.corner_radius = f.number_or("corner_radius", tool.corner_radius)?;
            if f.has("polygon_sides") {
                tool.polygon_sides = f.count("polygon_sides")?;
            }
            if f.has("star_points") {
                tool.star_points = f.count("star_points")?;
            }
            tool.star_inner = f.number_or("star_inner", tool.star_inner)?;
            v.set_vector_tool(tool);
            Ok(Value::Null)
        }
        other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    })
}

pub(super) fn run_proof(v: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Result<Value, String>, CommandRefusal> {
    let command = match id {
        "proof.select" => ProofCommand::Select(f.text("name")?),
        "proof.custom" => ProofCommand::Custom(folio_field!(f, "condition", folio::StudioProofConditionRecord, "a proof condition record")?),
        "proof.set_proof_colours" => ProofCommand::SetProofColours(f.flag("on")?),
        "proof.set_gamut_warning" => ProofCommand::SetGamutWarning(f.flag("on")?),
        "proof.set_gamut_style" => ProofCommand::SetGamutStyle { colour: f.vector::<3>("colour")?, opacity: f.count("opacity")? },
        "proof.select_colour_vision" => ProofCommand::SelectColourVision(f.token("vision", &[("protanopia", folio::ProofColourVision::Protanopia), ("deuteranopia", folio::ProofColourVision::Deuteranopia), ("tritanopia", folio::ProofColourVision::Tritanopia)])?),
        "proof.save_preset" => ProofCommand::SavePreset(f.text("name")?),
        other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    };
    Ok(done(v.proof_command(command)))
}

fn track_sizes(f: Fields<'_>, key: &str) -> Result<Vec<folio::StudioGridTrackSize>, CommandRefusal> {
    folio_field!(f, key, Vec<folio::StudioGridTrackSize>, "a list of track records")
}

pub(super) fn run_design(v: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Result<Value, String>, CommandRefusal> {
    let node = || f.text("node");
    let track_move = || Ok::<_, CommandRefusal>(GridTrackMove { from_index: f.count("from_index")?, to_index: f.count("to_index")? });
    Ok(done(match id {
        "design.set_auto_layout_mode" => v.set_auto_layout_mode(&node()?, folio_field!(f, "mode", folio::LayoutMode, "NONE|HORIZONTAL|VERTICAL|GRID")?),
        "design.set_auto_layout_spacing" => v.set_auto_layout_spacing(&node()?, f.number("item_spacing")?, f.vector::<4>("padding")?),
        "design.set_child_sizing" => v.set_child_sizing(&node()?, folio_field!(f, "horizontal", folio::ChildSizing, "FIXED|HUG|FILL")?, folio_field!(f, "vertical", folio::ChildSizing, "FIXED|HUG|FILL")?),
        "design.set_grid_child" => {
            let placement = if f.has("placement") { Some(folio_field!(f, "placement", folio::StudioGridChild, "a grid child record")?) } else { None };
            v.set_grid_child(&node()?, placement)
        }
        "design.set_grid_options" => v.set_grid_options(&node()?, folio_field!(f, "auto_tracks", folio::GridAutoTracks, "NONE|ROWS")?, folio_field!(f, "positioning", folio::GridItemsPositioning, "MANUAL|ROW_AUTO_FLOW")?),
        "design.set_grid_tracks" => v.set_grid_tracks(&node()?, &track_sizes(f, "rows")?, &track_sizes(f, "columns")?),
        "design.grid_reorder_rows" => v.grid_reorder_rows(&f.text("container")?, track_move()?),
        "design.grid_reorder_columns" => v.grid_reorder_columns(&f.text("container")?, track_move()?),
        other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    }))
}
