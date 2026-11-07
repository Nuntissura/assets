//! `panel.<panel key>.<action>`: every [`PanelAction`] by command id, through the same
//! [`Viewer::hosted_action`] the panel's control calls. A panel action on the selected layer takes that
//! selection; `viewer.select` sets it.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::hosted::{BrushChoice, PanelAction};
use super::super::vector::arrange::{Align, AlignTarget, Distribute, ReferencePoint, TransformEach, TransformOp};
use super::super::vector::envelope::EnvelopeSpec;
use super::super::vector::markers::MarkerSpec;
use super::super::vector::style::RowKind;
use super::super::vector::{LiveTextKind, PathEdit};
use super::CommandRefusal;
use super::decode::{Fields, folio_field, invalid, stroke_panel};
use hsk_studio_folio as folio;
use hsk_studio_nib::anchor_path::{AnchorAddr, Constraint, End, EndAddr, HandleSide};
use hsk_studio_nib::freeform::CornerTreatment;
use hsk_studio_nib::Mirroring;
use hsk_studio_nib::stroke::LineJoin;
use serde_json::{Value, json};

pub(super) fn row_kind(f: Fields<'_>, key: &str) -> Result<RowKind, CommandRefusal> {
    f.token(key, &[("fill", RowKind::Fill), ("stroke", RowKind::Stroke)])
}

pub(super) fn align_target(f: Fields<'_>) -> Result<AlignTarget, CommandRefusal> {
    Ok(match f.text("to")?.as_str() {
        "selection" => AlignTarget::Selection,
        "artboard" => AlignTarget::Artboard,
        "key_object" => AlignTarget::KeyObject(f.text("key_object")?),
        _ => return Err(invalid("to", "selection|artboard|key_object")),
    })
}

const ALIGN: &[(&str, Align)] =
    &[("left", Align::Left), ("centre_horizontal", Align::CentreHorizontal), ("right", Align::Right), ("top", Align::Top), ("centre_vertical", Align::CentreVertical), ("bottom", Align::Bottom)];

const REFERENCE: &[(&str, ReferencePoint)] = &[
    ("top_left", ReferencePoint::TopLeft),
    ("top", ReferencePoint::Top),
    ("top_right", ReferencePoint::TopRight),
    ("left", ReferencePoint::Left),
    ("centre", ReferencePoint::Centre),
    ("right", ReferencePoint::Right),
    ("bottom_left", ReferencePoint::BottomLeft),
    ("bottom", ReferencePoint::Bottom),
    ("bottom_right", ReferencePoint::BottomRight),
];

pub(super) fn align(f: Fields<'_>) -> Result<Align, CommandRefusal> {
    f.token("how", ALIGN)
}

/// `how` an edge or centre token, or `spacing` with `axis` 0 or 1 and an optional `gap`.
pub(super) fn distribute(f: Fields<'_>) -> Result<Distribute, CommandRefusal> {
    if f.opt_text("how")?.as_deref() == Some("spacing") {
        let axis = f.index("axis")?;
        if axis > 1 {
            return Err(invalid("axis", "0 (horizontal) or 1 (vertical)"));
        }
        return Ok(Distribute::Spacing { axis, gap: f.opt_number("gap")? });
    }
    align(f).map(Distribute::Edge)
}

pub(super) fn reference(f: Fields<'_>) -> Result<ReferencePoint, CommandRefusal> {
    if f.has("reference") { f.token("reference", REFERENCE) } else { Ok(ReferencePoint::Centre) }
}

/// `{"kind":"rotate","degrees":d}`, `scale` (horizontal, vertical percent), `reflect` (axis_deg), `shear`
/// (angle_deg, axis_deg) or `move` (dx, dy).
pub(super) fn transform_op(value: &Value, key: &str) -> Result<TransformOp, CommandRefusal> {
    let f = Fields::record(value, key, &["kind", "degrees", "horizontal", "vertical", "axis_deg", "angle_deg", "dx", "dy"])?;
    Ok(match f.text("kind")?.as_str() {
        "rotate" => TransformOp::Rotate(f.number("degrees")?),
        "scale" => TransformOp::Scale { horizontal: f.number("horizontal")?, vertical: f.number("vertical")? },
        "reflect" => TransformOp::Reflect { axis_deg: f.number("axis_deg")? },
        "shear" => TransformOp::Shear { angle_deg: f.number("angle_deg")?, axis_deg: f.number("axis_deg")? },
        "move" => TransformOp::Move { dx: f.number("dx")?, dy: f.number("dy")? },
        _ => return Err(invalid("kind", "rotate|scale|reflect|shear|move")),
    })
}

/// Unset fields keep [`TransformEach::default`].
pub(super) fn transform_each(value: &Value, key: &str) -> Result<TransformEach, CommandRefusal> {
    let f = Fields::record(value, key, &["scale_horizontal", "scale_vertical", "move_horizontal", "move_vertical", "rotate", "reflect_x", "reflect_y", "reference", "randomize", "seed"])?;
    let d = TransformEach::default();
    Ok(TransformEach {
        scale_horizontal: f.number_or("scale_horizontal", d.scale_horizontal)?,
        scale_vertical: f.number_or("scale_vertical", d.scale_vertical)?,
        move_horizontal: f.number_or("move_horizontal", d.move_horizontal)?,
        move_vertical: f.number_or("move_vertical", d.move_vertical)?,
        rotate: f.number_or("rotate", d.rotate)?,
        reflect_x: f.flag_or("reflect_x", d.reflect_x)?,
        reflect_y: f.flag_or("reflect_y", d.reflect_y)?,
        reference: reference(f)?,
        randomize: f.flag_or("randomize", d.randomize)?,
        seed: if f.has("seed") { f.unsigned("seed")? } else { d.seed },
    })
}

fn anchor(f: Fields<'_>) -> Result<AnchorAddr, CommandRefusal> {
    Ok(AnchorAddr { contour: f.index("contour")?, anchor: f.index("anchor")? })
}

fn constraint(f: Fields<'_>) -> Result<Constraint, CommandRefusal> {
    Ok(if f.flag_or("angle45", false)? { Constraint::Angle45 } else { Constraint::Free })
}

fn end_addr(value: &Value, key: &str) -> Result<EndAddr, CommandRefusal> {
    let f = Fields::record(value, key, &["contour", "end"])?;
    Ok(EndAddr { contour: f.index("contour")?, end: f.token("end", &[("start", End::Start), ("end", End::End)])? })
}

/// `{"kind": <PathEdit member>, ...its fields}`; one transaction each.
pub(super) fn path_edit(value: &Value, key: &str) -> Result<PathEdit, CommandRefusal> {
    let f = Fields::record(value, key, &["kind", "contour", "segment", "t", "anchor", "mirroring", "dx", "dy", "angle45", "side", "to", "break_mirroring", "a", "b", "corner", "radius"])?;
    Ok(match f.text("kind")?.as_str() {
        "insert_anchor" => PathEdit::InsertAnchor { contour: f.index("contour")?, segment: f.index("segment")?, t: f.number("t")? },
        "delete_anchor" => PathEdit::DeleteAnchor(anchor(f)?),
        "convert" => PathEdit::Convert(anchor(f)?, f.token("mirroring", &[("none", Mirroring::None), ("angle", Mirroring::Angle), ("angle_and_length", Mirroring::AngleAndLength)])?),
        "move_anchor" => PathEdit::MoveAnchor { at: anchor(f)?, dx: f.number("dx")?, dy: f.number("dy")?, constraint: constraint(f)? },
        "move_handle" => PathEdit::MoveHandle {
            at: anchor(f)?,
            side: f.token("side", &[("in", HandleSide::In), ("out", HandleSide::Out)])?,
            to: f.point("to")?,
            constraint: constraint(f)?,
            break_mirroring: f.flag_or("break_mirroring", false)?,
        },
        "bend" => PathEdit::Bend { contour: f.index("contour")?, segment: f.index("segment")?, t: f.number("t")?, dx: f.number("dx")?, dy: f.number("dy")? },
        "split_at" => PathEdit::SplitAt { contour: f.index("contour")?, segment: f.index("segment")?, t: f.number("t")? },
        "split_at_anchor" => PathEdit::SplitAtAnchor(anchor(f)?),
        "join" => {
            let (a, b) = (f.json("a", "an end")?, f.json("b", "an end")?);
            PathEdit::Join(end_addr(&a, "a")?, end_addr(&b, "b")?)
        }
        "reverse" => PathEdit::Reverse(if f.has("contour") { Some(f.index("contour")?) } else { None }),
        "corner" => PathEdit::Corner {
            at: anchor(f)?,
            kind: f.token("corner", &[("round", CornerTreatment::Round), ("inverted_round", CornerTreatment::InvertedRound), ("chamfer", CornerTreatment::Chamfer)])?,
            radius: f.number("radius")?,
        },
        _ => return Err(invalid("kind", "insert_anchor|delete_anchor|convert|move_anchor|move_handle|bend|split_at|split_at_anchor|join|reverse|corner")),
    })
}

/// `{"kind":"warp","style":...,"axis":...,"bend":...,"horizontal":...,"vertical":...}`,
/// `{"kind":"mesh","rows":r,"columns":c}` or `{"kind":"top_object","layer":id}`.
pub(super) fn envelope_spec(value: &Value, key: &str) -> Result<EnvelopeSpec, CommandRefusal> {
    let f = Fields::record(value, key, &["kind", "style", "axis", "bend", "horizontal", "vertical", "rows", "columns", "layer"])?;
    Ok(match f.text("kind")?.as_str() {
        "warp" => EnvelopeSpec::Warp {
            style: folio_field!(f, "style", folio::StudioWarpStyle, "a warp style token")?,
            axis: folio_field!(f, "axis", folio::StudioWarpAxis, "horizontal|vertical")?,
            bend: f.number("bend")?,
            horizontal: f.number_or("horizontal", 0.0)?,
            vertical: f.number_or("vertical", 0.0)?,
        },
        "mesh" => {
            // The record's own limit, checked before a grid of that size is built.
            let side = |key: &str| f.count(key).and_then(|n| if (1..=64).contains(&n) { Ok(n) } else { Err(invalid(key, "1..=64")) });
            EnvelopeSpec::Mesh { rows: side("rows")?, columns: side("columns")? }
        }
        "top_object" => EnvelopeSpec::TopObject { layer: f.text("layer")? },
        _ => return Err(invalid("kind", "warp|mesh|top_object")),
    })
}

fn arrowhead(f: Fields<'_>, key: &str) -> Result<folio::StudioArrowhead, CommandRefusal> {
    folio::StudioArrowhead::from_token(&f.text(key)?).ok_or_else(|| invalid(key, "an arrowhead token of the catalogue"))
}

/// `{"start":token,"end":token,"alignment":token,"start_scale":pct,"end_scale":pct}`; scales default to
/// Folio's default.
pub(super) fn marker_spec(value: &Value, key: &str) -> Result<MarkerSpec, CommandRefusal> {
    let f = Fields::record(value, key, &["start", "end", "alignment", "start_scale", "end_scale"])?;
    Ok(MarkerSpec {
        start: arrowhead(f, "start")?,
        end: arrowhead(f, "end")?,
        start_scale: f.number_or("start_scale", folio::DEFAULT_MARKER_SCALE)?,
        end_scale: f.number_or("end_scale", folio::DEFAULT_MARKER_SCALE)?,
        alignment: folio_field!(f, "alignment", folio::StudioArrowAlignment, "arrow_align.inside_path|arrow_align.outside_path")?,
    })
}

/// The typed action of one `panel.*` row. The panel key is the id's middle part, for the catalog; the
/// action alone picks the member.
fn action_of(id: &str, f: Fields<'_>) -> Result<PanelAction, CommandRefusal> {
    let action = id.rsplit('.').next().unwrap_or_default();
    Ok(match action {
        "set_fill" => PanelAction::SetFill(f.opt_rgb8("rgb8")?),
        "clear_appearance" => PanelAction::ClearAppearance,
        "reduce_to_basic_appearance" => PanelAction::ReduceToBasicAppearance,
        "set_stroke" => PanelAction::SetStroke(if f.has("stroke") { Some(stroke_panel(&f.json("stroke", "a stroke panel")?, "stroke")?) } else { None }),
        "set_stroke_markers" => PanelAction::SetStrokeMarkers(if f.has("markers") { Some(marker_spec(&f.json("markers", "markers")?, "markers")?) } else { None }),
        "swap_stroke_markers" => PanelAction::SwapStrokeMarkers,
        "add_swatch" => PanelAction::AddSwatch { name: f.text("name")?, colour: f.rgb8("rgb8")? },
        "apply_swatch" => PanelAction::ApplySwatch { swatch_id: f.text("swatch")?, kind: row_kind(f, "kind")?, tint: f.opt_number("tint")? },
        "detach_swatch" => PanelAction::DetachSwatch(row_kind(f, "kind")?),
        "add_graphic_style" => PanelAction::AddGraphicStyle { name: f.text("name")? },
        "apply_graphic_style" => PanelAction::ApplyGraphicStyle(f.text("style")?),
        "merge_graphic_style" => PanelAction::MergeGraphicStyle(f.text("style")?),
        "redefine_graphic_style" => PanelAction::RedefineGraphicStyle(f.text("style")?),
        "break_graphic_style" => PanelAction::BreakGraphicStyle,
        "align" => PanelAction::Align { layers: f.list("layers")?, to: align_target(f)?, how: self::align(f)? },
        "distribute" => PanelAction::Distribute { layers: f.list("layers")?, to: align_target(f)?, how: distribute(f)? },
        "transform" => PanelAction::Transform { layers: f.list("layers")?, op: transform_op(&f.json("op", "a transform")?, "op")?, reference: reference(f)? },
        "transform_each" => PanelAction::TransformEach { layers: f.list("layers")?, each: transform_each(&f.json("each", "a transform-each record")?, "each")? },
        "offset_path" => PanelAction::OffsetPath { offset: f.number("offset")?, join: f.token("join", &[("miter", LineJoin::Miter), ("round", LineJoin::Round), ("bevel", LineJoin::Bevel)])? },
        "outline_stroke" => PanelAction::OutlineStroke,
        "expand_shape" => PanelAction::ExpandShape,
        "promote_network" => PanelAction::PromoteNetwork,
        "edit_path" => PanelAction::EditPath(path_edit(&f.json("edit", "a path edit")?, "edit")?),
        "bind_live_text" => {
            let kind = match f.text("kind")?.as_str() {
                "path" => LiveTextKind::Path,
                "area" => LiveTextKind::Area { inset_pt: f.number_or("inset_pt", 0.0)? },
                _ => return Err(invalid("kind", "path|area")),
            };
            PanelAction::BindLiveText { text: f.text("text")?, kind }
        }
        "convert_text_kind" => PanelAction::ConvertTextKind,
        "create_outlines" => PanelAction::CreateOutlines,
        "make_clip" => PanelAction::MakeClip { source: f.text("source")?, even_odd: f.flag_or("even_odd", false)? },
        "make_mask" => PanelAction::MakeMask { source: f.text("source")?, mode: folio_field!(f, "mode", folio::StudioMaskMode, "ALPHA|VECTOR|LUMINANCE")?, invert: f.flag_or("invert", false)? },
        "release_masks" => PanelAction::ReleaseMasks,
        "make_envelope" => PanelAction::MakeEnvelope { spec: envelope_spec(&f.json("spec", "an envelope")?, "spec")?, fidelity: f.number_or("fidelity", 100.0)?, distort_appearance: f.flag_or("distort_appearance", false)? },
        "release_envelope" => PanelAction::ReleaseEnvelope,
        "expand_envelope" => PanelAction::ExpandEnvelope,
        "add_brush" => PanelAction::AddBrush {
            name: f.text("name")?,
            kind: f.token("kind", &[("art", BrushChoice::Art), ("scatter", BrushChoice::Scatter), ("pattern", BrushChoice::Pattern)])?,
            colorization: folio_field!(f, "colorization", folio::StudioBrushColorization, "none|tints|tints_and_shades|hue_shift")?,
            layers: f.list("layers")?,
        },
        "apply_brush" => PanelAction::ApplyBrush { brush_id: f.text("brush")?, scale: f.opt_number("scale")? },
        "remove_brush" => PanelAction::RemoveBrush,
        "export_svg" => PanelAction::ExportSvg { path: f.path("path")?, overwrite: f.flag_or("overwrite", false)? },
        "export_pdf" => PanelAction::ExportPdf { path: f.path("path")?, overwrite: f.flag_or("overwrite", false)? },
        "import_vector" => PanelAction::ImportVector { source: f.path("source")?, dest: f.path("dest")?, overwrite: f.flag_or("overwrite", false)? },
        #[cfg(feature = "audio-batch")]
        "batch_add" => {
            use super::super::audio_import::MixFormat;
            let bits = || -> Result<u8, CommandRefusal> { u8::try_from(f.unsigned("bits")?).map_err(|_| invalid("bits", "16 or 24")) };
            let format = match f.text("format")?.as_str() {
                "flac" => MixFormat::Flac { bits: bits()? },
                "aiff" => MixFormat::Aiff { bits: bits()? },
                "aiff_float" => MixFormat::AiffFloat,
                _ => return Err(invalid("format", "flac|aiff|aiff_float")),
            };
            PanelAction::BatchAdd(super::super::hosted::BatchItem { input: f.path("input")?, output: f.path("output")?, gain_db: f.opt_number("gain_db")?, format, overwrite: f.flag_or("overwrite", false)? })
        }
        #[cfg(feature = "audio-batch")]
        "batch_run" => PanelAction::BatchRun,
        #[cfg(feature = "audio-batch")]
        "batch_clear" => PanelAction::BatchClear,
        other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": id, "action": other, "reason": "this build has no such panel action"}))),
    })
}

pub(super) fn run(viewer: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Value, CommandRefusal> {
    let action = action_of(id, f)?;
    let before = viewer.run().ops().len();
    viewer.hosted_action(action).map(Value::String).map_err(|message| super::method_refusal(viewer, before, message))
}
