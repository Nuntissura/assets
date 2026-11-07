//! `viewer.<method>` of the process drive surface (lane SMB-HARNESS-DRIVE-1): the Viewer's non-draw
//! methods whose arguments have a text or JSON form, one row of `manual::DRIVE_VIEWER` each. The id
//! is the method's own name; attribution comes from the viewer's run context, like every window
//! action. Methods that take a whole typed record run through `Viewer::run_command` instead.
#![deny(clippy::arithmetic_side_effects)]

use super::drive::{Params, Refusal, refuse};
use crate::view::Viewer;
use crate::view::edit::{BrushSettings, Tool};
use crate::view::export::ExportFormat;
use crate::view::layout_export::{self, LayoutExportRefusal, LayoutPdfSettings, PdfXStandard};
use crate::view::vector::style::RowKind;
use crate::view::vector::{LiveTextKind, ShapeKind};
use crate::Place;
use hsk_studio_harness_core::document_new::{self, ArtboardOptionsEdit, BackgroundFill, DocumentProfile, NewDocument};
use hsk_studio_harness_core::manual::DriveViewer;
use hsk_studio_nib::anchor_path::Constraint;
use hsk_studio_nib::pathfinder::PathfinderOp;
use hsk_studio_nib::stroke::LineJoin;
use hsk_studio_pigment::brush::StrokeSample;
use hsk_studio_pigment::select_tools::wand::WandOptions;
use hsk_studio_pigment::selection::CombineMode;
use serde_json::Value;
use std::path::Path;

/// Typed access to a row's text parameters: a missing or undecodable value is a typed refusal.
struct Args<'a>(&'a Params);

fn invalid(key: &str, expected: &str) -> Refusal {
    refuse("parameter_invalid", serde_json::json!({"key": key, "expected": expected}))
}

fn finite(value: &Value) -> Option<f64> {
    value.as_f64().filter(|n| n.is_finite())
}

fn xy(value: &Value) -> Option<[f64; 2]> {
    match value.as_array()?.as_slice() {
        [x, y] => Some([finite(x)?, finite(y)?]),
        _ => None,
    }
}

impl Args<'_> {
    fn opt(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }
    fn text(&self, key: &str) -> Result<&str, Refusal> {
        self.opt(key).ok_or_else(|| refuse("parameter_missing", serde_json::json!({"key": key})))
    }
    fn parsed<T: std::str::FromStr>(&self, key: &str, expected: &str) -> Result<T, Refusal> {
        self.text(key)?.parse().map_err(|_| invalid(key, expected))
    }
    fn number(&self, key: &str) -> Result<f64, Refusal> {
        let value: f64 = self.parsed(key, "finite number")?;
        if value.is_finite() { Ok(value) } else { Err(invalid(key, "finite number")) }
    }
    fn opt_number(&self, key: &str) -> Result<Option<f64>, Refusal> {
        self.opt(key).map(|_| self.number(key)).transpose()
    }
    fn flag(&self, key: &str) -> Result<bool, Refusal> {
        self.parsed(key, "boolean")
    }
    fn flag_or(&self, key: &str, default: bool) -> Result<bool, Refusal> {
        self.opt(key).map_or(Ok(default), |_| self.flag(key))
    }
    /// Comma-separated text, or a JSON list of strings (`[]` is the empty list).
    fn list(&self, key: &str) -> Result<Vec<String>, Refusal> {
        let text = self.text(key)?;
        if text.trim_start().starts_with('[') {
            let items: Vec<Value> = serde_json::from_str(text).map_err(|_| invalid(key, "a list of text"))?;
            return items.iter().map(|v| v.as_str().map(str::to_string)).collect::<Option<_>>().ok_or_else(|| invalid(key, "a list of text"));
        }
        Ok(text.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect())
    }
    fn json(&self, key: &str, expected: &str) -> Result<Value, Refusal> {
        serde_json::from_str(self.text(key)?).map_err(|_| invalid(key, expected))
    }
    fn point(&self, key: &str) -> Result<[f64; 2], Refusal> {
        xy(&self.json(key, "[x, y]")?).ok_or_else(|| invalid(key, "[x, y]"))
    }
    fn opt_point(&self, key: &str) -> Result<Option<[f64; 2]>, Refusal> {
        self.opt(key).map(|_| self.point(key)).transpose()
    }
    fn points(&self, key: &str) -> Result<Vec<[f64; 2]>, Refusal> {
        let value = self.json(key, "[[x, y], ...]")?;
        value.as_array().and_then(|items| items.iter().map(xy).collect()).ok_or_else(|| invalid(key, "[[x, y], ...]"))
    }
    /// `[[x, y], ...]`, or `[[x, y, pressure], ...]`.
    fn samples(&self, key: &str) -> Result<Vec<StrokeSample>, Refusal> {
        let expected = "[[x, y] or [x, y, pressure], ...]";
        let value = self.json(key, expected)?;
        let sample = |v: &Value| match v.as_array()?.as_slice() {
            [x, y] => Some(StrokeSample { x: finite(x)?, y: finite(y)?, pressure: None, time_ms: None }),
            #[allow(clippy::cast_possible_truncation)]
            [x, y, p] => Some(StrokeSample { x: finite(x)?, y: finite(y)?, pressure: Some(finite(p)? as f32), time_ms: None }),
            _ => None,
        };
        value.as_array().and_then(|items| items.iter().map(sample).collect()).ok_or_else(|| invalid(key, expected))
    }
    fn token(&self, key: &str, tokens: &[&str]) -> Result<usize, Refusal> {
        let given = self.text(key)?;
        tokens.iter().position(|t| *t == given).ok_or_else(|| invalid(key, &tokens.join("|")))
    }
    fn path(&self, key: &str) -> Result<&Path, Refusal> {
        self.text(key).map(Path::new)
    }
    fn row_kind(&self, key: &str) -> Result<RowKind, Refusal> {
        Ok(if self.token(key, &["fill", "stroke"])? == 0 { RowKind::Fill } else { RowKind::Stroke })
    }
    fn place(&self) -> Result<Place, Refusal> {
        let target = || self.text("target").map(str::to_string);
        Ok(match self.token("place", &["inside", "at_beginning", "at_end", "before", "after"])? {
            0 => Place::Inside(target()?),
            1 => Place::AtBeginning(self.opt("target").map(str::to_string)),
            2 => Place::AtEnd(self.opt("target").map(str::to_string)),
            3 => Place::Before(target()?),
            _ => Place::After(target()?),
        })
    }
}

/// A rule's stable code as a refusal before the method ran.
fn coded(code: &'static str) -> Refusal {
    refuse(code, Value::Null)
}

/// `width` and `height`: both, neither, or `size_incomplete`.
fn size_of(a: &Args<'_>) -> Result<Option<(f64, f64)>, Refusal> {
    match (a.opt_number("width")?, a.opt_number("height")?) {
        (Some(w), Some(h)) => Ok(Some((w, h))),
        (None, None) => Ok(None),
        _ => Err(coded("size_incomplete")),
    }
}

/// The request of `document.new`; a bad token is its typed refusal.
fn new_document_request(a: &Args<'_>) -> Result<NewDocument, Refusal> {
    let mut spec = NewDocument::new(DocumentProfile::parse(a.text("profile")?).map_err(coded)?, size_of(a)?);
    if let Some(unit) = a.opt("unit") {
        spec.unit = document_new::parse_unit(unit).map_err(coded)?;
    }
    if let Some(mode) = a.opt("colour_mode") {
        spec.colour_mode = document_new::parse_colour_mode(mode).map_err(coded)?;
    }
    if a.opt("bits_per_channel").is_some() {
        spec.bits_per_channel = a.parsed("bits_per_channel", "1, 8, 16 or 32")?;
    }
    spec.artboard_name = a.opt("name").map(str::to_string);
    Ok(spec)
}

fn options_edit(a: &Args<'_>) -> Result<ArtboardOptionsEdit, Refusal> {
    let flag = |key: &str| a.opt(key).map(|_| a.flag(key)).transpose();
    Ok(ArtboardOptionsEdit {
        ruler_origin: a.opt_point("ruler_origin")?,
        ruler_pixel_aspect_ratio: a.opt_number("ruler_pixel_aspect_ratio")?,
        show_center_mark: flag("show_center_mark")?,
        show_cross_hairs: flag("show_cross_hairs")?,
        show_safe_areas: flag("show_safe_areas")?,
        fade_region_outside_artboard: flag("fade_region_outside_artboard")?,
        update_while_dragging: flag("update_while_dragging")?,
        background_fill: a.opt("background_fill").map(BackgroundFill::parse).transpose().map_err(coded)?,
        preset: a.opt("preset").map(str::to_string),
    })
}

/// A layout output method's typed refusal as the drive's refusal (its own code).
fn layout(result: Result<Value, LayoutExportRefusal>) -> Result<Outcome, Refusal> {
    result.map(Ok).map_err(|r| refuse(r.code, serde_json::json!({"message": r.detail})))
}

/// The output-intent ICC bytes of `path`, bounded.
fn read_profile(path: &Path) -> Result<Vec<u8>, LayoutExportRefusal> {
    layout_export::read_bounded(path, layout_export::MAX_PROFILE_BYTES, "output_profile_unreadable")
}

/// The outcome of a viewer method: its value, or its refusal text.
type Outcome = Result<Value, String>;

fn plain<T: Into<Value>>(result: Result<T, String>) -> Outcome {
    result.map(Into::into)
}

fn unit(result: Result<(), String>) -> Outcome {
    result.map(|()| Value::Null)
}

/// Every given key is declared, every required key is given.
fn check_row(row: &DriveViewer, params: &Params) -> Result<(), Refusal> {
    if let Some(key) = params.keys().find(|k| !row.params.iter().any(|p| p.key == *k)) {
        return Err(refuse("parameter_unknown", serde_json::json!({"key": key})));
    }
    if let Some(p) = row.params.iter().find(|p| p.required && !params.contains_key(p.key)) {
        return Err(refuse("parameter_missing", serde_json::json!({"key": p.key})));
    }
    Ok(())
}

/// Runs one manual row on the viewer. The outer error is a refusal before the method ran; the inner
/// one the method's own refusal.
pub(super) fn call(viewer: &mut Viewer, row: &DriveViewer, params: &Params) -> Result<Outcome, Refusal> {
    check_row(row, params)?;
    let a = Args(params);
    let layer = || a.text("layer");
    Ok(match row.method {
        // ---- document, artboards, pages (lane SMB-DOC-NEW-1) ----
        "new_document" => plain(viewer.new_document(&new_document_request(&a)?, a.opt("file_name"))),
        "add_artboard" => {
            let at = match (a.opt_number("x")?, a.opt_number("y")?) {
                (Some(x), Some(y)) => Some([x, y]),
                (None, None) => None,
                _ => return Err(coded("position_incomplete")),
            };
            plain(viewer.add_artboard(a.opt("name").unwrap_or_default(), a.number("width")?, a.number("height")?, at))
        }
        "rename_artboard" => plain(viewer.rename_artboard(a.text("artboard")?, a.text("name")?)),
        "resize_artboard" => plain(viewer.resize_artboard(a.text("artboard")?, a.number("width")?, a.number("height")?)),
        "move_artboard" => plain(viewer.move_artboard(a.text("artboard")?, a.number("x")?, a.number("y")?)),
        "delete_artboard" => plain(viewer.delete_artboard(a.text("artboard")?)),
        "set_artboard_options" => plain(viewer.set_artboard_options(a.text("artboard")?, &options_edit(&a)?)),
        "go_to_artboard" => unit(viewer.go_to_artboard(a.text("artboard")?)),
        "add_page" => plain(viewer.add_page(a.opt("after"), a.flag_or("new_spread", false)?, size_of(&a)?, a.opt("name"))),
        "rename_page" => plain(viewer.rename_page(a.text("page")?, a.text("name")?)),
        "resize_page" => plain(viewer.resize_page(a.text("page")?, a.number("width")?, a.number("height")?)),
        "delete_page" => plain(viewer.delete_page(a.text("page")?)),
        "go_to_page" => unit(viewer.go_to_page(a.text("page")?)),
        // ---- layers ----
        "new_raster_layer" => plain(viewer.new_raster_layer(a.text("name")?)),
        "duplicate_layer" => plain(viewer.duplicate_layer(layer()?)),
        "delete_layer" => plain(viewer.delete_layer(layer()?)),
        "shift_layer" => plain(viewer.shift_layer(layer()?, a.flag("up")?)),
        "ungroup" => plain(viewer.ungroup(a.text("group")?)),
        "group_layers" => plain(viewer.group_layers(&a.list("layers")?, a.text("name")?)),
        "move_layer" => plain(viewer.move_layer(layer()?, a.place()?)),
        "select" => {
            viewer.select(a.opt("layer").map(str::to_string));
            Ok(Value::Null)
        }
        // ---- tools, selection, paint, fill ----
        "set_tool" => {
            let key = a.text("tool")?;
            let tool = Tool::ALL.into_iter().find(|t| t.key() == key).ok_or_else(|| invalid("tool", "a key of state.edit.tools"))?;
            viewer.set_tool(tool);
            Ok(Value::Null)
        }
        "set_combine" => {
            let modes = [CombineMode::Replace, CombineMode::Extend, CombineMode::Diminish, CombineMode::Intersect];
            let index = a.token("mode", &["replace", "extend", "diminish", "intersect"])?;
            viewer.set_combine(modes[index]);
            Ok(Value::Null)
        }
        "set_brush" => {
            let base = BrushSettings::default();
            let rgb8 = match a.opt("rgb8") {
                None => base.rgb8,
                Some(_) => a.json("rgb8", "[r, g, b]")?.as_array().and_then(|c| match c.as_slice() {
                    [r, g, b] => Some([u8::try_from(r.as_u64()?).ok()?, u8::try_from(g.as_u64()?).ok()?, u8::try_from(b.as_u64()?).ok()?]),
                    _ => None,
                }).ok_or_else(|| invalid("rgb8", "[r, g, b]"))?,
            };
            viewer.set_brush(BrushSettings {
                diameter_px: a.opt_number("diameter_px")?.unwrap_or(base.diameter_px),
                hardness_pct: a.opt_number("hardness_pct")?.unwrap_or(base.hardness_pct),
                spacing_pct: a.opt_number("spacing_pct")?.unwrap_or(base.spacing_pct),
                opacity_pct: a.opt_number("opacity_pct")?.unwrap_or(base.opacity_pct),
                rgb8,
            });
            Ok(Value::Null)
        }
        "select_rect" => unit(viewer.select_rect(a.number("x")?, a.number("y")?, a.number("width")?, a.number("height")?)),
        "select_ellipse" => unit(viewer.select_ellipse(a.number("x")?, a.number("y")?, a.number("width")?, a.number("height")?)),
        "select_lasso" => unit(viewer.select_lasso(&a.points("points")?)),
        "clear_selection" => {
            viewer.clear_selection();
            Ok(Value::Null)
        }
        "undo_selection" => unit(viewer.undo_selection()),
        "select_wand" => {
            let base = WandOptions::default();
            let options = WandOptions { tolerance: a.opt_number("tolerance")?.unwrap_or(base.tolerance), contiguous: a.flag_or("contiguous", base.contiguous)?, ..base };
            unit(viewer.select_wand(layer()?, a.parsed("x", "integer")?, a.parsed("y", "integer")?, &options))
        }
        "ps_click" => unit(viewer.ps_click(a.number("x")?, a.number("y")?)),
        "refine_end" => {
            viewer.refine_end();
            Ok(Value::Null)
        }
        "paint" => plain(viewer.paint(layer()?, &a.samples("samples")?)),
        "fill_click" => plain(viewer.fill_click(a.number("x")?, a.number("y")?)),
        "fill_drag" => plain(viewer.fill_drag(a.point("from")?, a.point("to")?)),
        "info_eyedropper" => viewer.info_eyedropper(a.number("x")?, a.number("y")?),
        // ---- develop and export ----
        "set_develop" => match viewer.set_develop(a.text("control")?, a.number("value")?) {
            Ok(revision) => Ok(revision.into()),
            Err(r) => return Err(refuse(r.code, serde_json::json!({"message": r.detail}))),
        },
        "set_develop_threads" => match viewer.set_develop_threads(a.parsed("count", "thread count")?) {
            Ok(()) => Ok(Value::Null),
            Err(r) => return Err(refuse(r.code, serde_json::json!({"message": r.detail}))),
        },
        "set_develop_export_field" => match viewer.set_develop_export_field(a.text("id")?, a.text("value")?) {
            Ok(()) => Ok(Value::Null),
            Err(r) => return Err(refuse(r.code, serde_json::json!({"message": r.detail}))),
        },
        "load_develop_watermark" => match viewer.load_develop_watermark(a.path("path")?) {
            Ok(()) => Ok(Value::Null),
            Err(r) => return Err(refuse(r.code, serde_json::json!({"message": r.detail}))),
        },
        "clear_develop_export_stage" => match viewer.clear_develop_export_stage() {
            Ok(()) => Ok(Value::Null),
            Err(r) => return Err(refuse(r.code, serde_json::json!({"message": r.detail}))),
        },
        "export" => {
            let format = match a.token("format", &["png", "jpeg"])? {
                0 => ExportFormat::Png,
                _ => ExportFormat::Jpeg { quality: a.opt("quality").map_or(Ok(crate::view::export::DEFAULT_JPEG_QUALITY), |_| a.parsed("quality", "1..=100"))? },
            };
            viewer.export(a.path("path")?, format, a.flag_or("overwrite", false)?)
        }
        // The recipe is JSON text (an object in the JSONL line); a recipe that does not decode is the method's
        // typed refusal (recipe_not_json and the recipe codes), relative file names resolve against the cwd.
        "export_document" => {
            let cancel = hsk_studio_accord::CancellationToken::default();
            let (path, overwrite) = (a.path("path")?, a.flag_or("overwrite", false)?);
            let recipe = serde_json::from_str::<Value>(a.text("recipe")?)
                .map_err(|_| crate::view::develop::Refusal { code: "recipe_not_json", detail: "recipe".into() })
                .and_then(|v| crate::view::doc_export::DocExportRecipe::from_json(&v, None, &cancel));
            match recipe.and_then(|r| viewer.export_document(path, &r, overwrite)) {
                Ok(report) => Ok(report),
                Err(r) => return Err(refuse(r.code, serde_json::json!({"message": r.detail}))),
            }
        }
        "export_vector_svg" => plain(viewer.export_vector_svg(a.path("path")?, a.flag_or("overwrite", false)?)),
        "export_vector_pdf" => plain(viewer.export_vector_pdf(a.path("path")?, a.flag_or("overwrite", false)?)),
        // ---- layout output (lane SMB-LAYOUT-PDF-HOST-1) ----
        "layout_open" => {
            let size = match (a.opt_number("page_width_pt")?, a.opt_number("page_height_pt")?) {
                (Some(w), Some(h)) => Some((w, h)),
                (None, None) => None,
                _ => return Err(refuse("page_size_incomplete", serde_json::json!({"message": "give page_width_pt and page_height_pt together"}))),
            };
            let pages = a.opt("pages").map(|_| a.parsed::<u32>("pages", "page count")).transpose()?;
            layout(viewer.layout_open(size, pages))?
        }
        "export_layout_pdf" => {
            let profile = match a.opt("output_profile") {
                Some(path) => Some(read_profile(Path::new(path)).map_err(|r| refuse(r.code, serde_json::json!({"message": r.detail})))?),
                None => None,
            };
            let pdfx = match a.opt("pdfx") {
                None => PdfXStandard::None,
                Some(_) => if a.token("pdfx", &["pdfx.none", "pdfx.x4_2010"])? == 0 { PdfXStandard::None } else { PdfXStandard::X4_2010 },
            };
            let settings = LayoutPdfSettings {
                pdfx,
                created: a.opt("created"),
                output_profile: profile.as_deref(),
                output_condition_identifier: a.opt("output_condition_identifier"),
                output_condition: a.opt("output_condition"),
                registry: a.opt("registry"),
                title: a.opt("title"),
                subset_fonts: a.flag_or("subset_fonts", true)?,
                ignore_preflight: a.flag_or("ignore_preflight", false)?,
            };
            layout(viewer.export_layout_pdf(a.path("path")?, a.flag_or("overwrite", false)?, &settings))?
        }
        "export_layout_idml" => layout(viewer.export_layout_idml(a.path("path")?, a.flag_or("overwrite", false)?))?,
        // ---- vector tools ----
        "pen_point" => plain(viewer.pen_point(a.number("x")?, a.number("y")?, a.opt_point("drag")?, a.number("close_radius")?)),
        "pen_end" => {
            viewer.pen_end();
            Ok(Value::Null)
        }
        "curvature_point" => plain(viewer.curvature_point(a.number("x")?, a.number("y")?, a.flag("corner")?, a.number("close_radius")?)),
        "draw_shape" => {
            let kinds = [ShapeKind::Rectangle, ShapeKind::RoundedRectangle, ShapeKind::Ellipse, ShapeKind::Polygon, ShapeKind::Star, ShapeKind::Line, ShapeKind::ArcSegment, ShapeKind::RectangularGrid, ShapeKind::PolarGrid, ShapeKind::Spiral];
            let index = a.token("kind", &["rectangle", "rounded_rectangle", "ellipse", "polygon", "star", "line", "arc_segment", "rectangular_grid", "polar_grid", "spiral"])?;
            plain(viewer.draw_shape(kinds[index], a.point("a")?, a.point("b")?, a.flag_or("constrain", false)?))
        }
        "pencil_stroke" => plain(viewer.pencil_stroke(&a.points("points")?)),
        "blob_brush_stroke" => plain(viewer.blob_brush_stroke(&a.points("points")?)),
        "knife_cut" => plain(viewer.knife_cut(&a.points("points")?)),
        "direct_drag" => {
            let constraint = if a.flag_or("angle45", false)? { Constraint::Angle45 } else { Constraint::Free };
            plain(viewer.direct_drag(a.point("from")?, a.point("to")?, constraint, a.flag_or("break_mirroring", false)?, a.number("radius")?))
        }
        "reshape_drag" => plain(viewer.reshape_drag(a.point("from")?, a.point("to")?, a.number("radius")?)),
        "width_drag" => plain(viewer.width_drag(a.point("from")?, a.point("to")?, a.number("radius")?)),
        "set_width_point" => plain(viewer.set_width_point(layer()?, a.number("t")?, a.number("width")?)),
        "clear_width" => plain(viewer.clear_width(layer()?)),
        "expand_shape" => plain(viewer.expand_shape(layer()?)),
        "promote_network" => plain(viewer.promote_network(layer()?)),
        "outline_stroke_layer" => plain(viewer.outline_stroke_layer(layer()?)),
        "offset_path_layer" => {
            let joins = [LineJoin::Miter, LineJoin::Round, LineJoin::Bevel];
            plain(viewer.offset_path_layer(layer()?, a.number("offset")?, joins[a.token("join", &["miter", "round", "bevel"])?]))
        }
        "create_outlines" => plain(viewer.create_outlines(layer()?)),
        "convert_text_kind" => plain(viewer.convert_text_kind(layer()?)),
        "bind_live_text" => {
            let kind = if a.token("kind", &["path", "area"])? == 0 { LiveTextKind::Path } else { LiveTextKind::Area { inset_pt: a.opt_number("inset_pt")?.unwrap_or(0.0) } };
            plain(viewer.bind_live_text(layer()?, a.text("text")?, kind))
        }
        "text_click" => plain(viewer.text_click(a.point("p")?, a.number("radius")?, a.text("text")?)),
        "network_paint_bucket" => plain(viewer.network_paint_bucket(layer()?, a.point("p")?, a.flag("fill")?)),
        "network_region_swatch" => plain(viewer.network_region_swatch(layer()?, a.point("p")?, a.opt("swatch"))),
        "network_pen" => plain(viewer.network_pen(layer()?, a.number("x")?, a.number("y")?, a.opt_point("drag")?, a.number("radius")?)),
        "pathfinder_layers" => {
            let ops = [
                PathfinderOp::Union, PathfinderOp::Intersect, PathfinderOp::Exclude, PathfinderOp::SubtractFront, PathfinderOp::SubtractBack, PathfinderOp::Divide, PathfinderOp::Trim,
                PathfinderOp::Merge, PathfinderOp::Crop, PathfinderOp::Outline, PathfinderOp::HardMix, PathfinderOp::SoftMix, PathfinderOp::Trap,
            ];
            let index = a.token("op", &["union", "intersect", "exclude", "subtract_front", "subtract_back", "divide", "trim", "merge", "crop", "outline", "hard_mix", "soft_mix", "trap"])?;
            plain(viewer.pathfinder_layers(ops[index], &a.list("layers")?))
        }
        "shape_builder_layers" => plain(viewer.shape_builder_layers(&a.list("layers")?, &a.points("gesture")?, a.flag("delete")?)),
        "make_clip" => plain(viewer.make_clip(a.text("target")?, a.text("source")?, a.flag_or("even_odd", false)?)),
        "release_masks" => unit(viewer.release_masks(a.text("target")?)),
        "expand_envelope" => plain(viewer.expand_envelope(layer()?)),
        "release_envelope" => plain(viewer.release_envelope(layer()?)),
        "move_envelope_point" => plain(viewer.move_envelope_point(layer()?, a.parsed("index", "index")?, a.point("to")?)),
        "create_gradient_mesh" => plain(viewer.create_gradient_mesh(layer()?, a.parsed("rows", "count")?, a.parsed("columns", "count")?)),
        // ---- appearance ----
        "clear_appearance" => plain(viewer.clear_appearance(layer()?)),
        "break_graphic_style" => plain(viewer.break_graphic_style(layer()?)),
        "reduce_to_basic_appearance" => plain(viewer.reduce_to_basic_appearance(layer()?)),
        "add_graphic_style" => plain(viewer.add_graphic_style(a.text("name")?, layer()?)),
        "apply_graphic_style" => plain(viewer.apply_graphic_style(layer()?, a.text("style")?)),
        "merge_graphic_style" => plain(viewer.merge_graphic_style(layer()?, a.text("style")?)),
        "redefine_graphic_style" => plain(viewer.redefine_graphic_style(a.text("style")?, layer()?)),
        "apply_brush" => plain(viewer.apply_brush(layer()?, a.text("brush")?, a.opt_number("scale")?)),
        "remove_brush" => plain(viewer.remove_brush(layer()?)),
        "swap_stroke_markers" => plain(viewer.swap_stroke_markers(layer()?, a.parsed("row", "row")?)),
        "apply_swatch" => plain(viewer.apply_swatch(layer()?, a.text("swatch")?, a.row_kind("kind")?)),
        "apply_swatch_tinted" => plain(viewer.apply_swatch_tinted(layer()?, a.text("swatch")?, a.row_kind("kind")?, a.opt_number("tint")?)),
        "detach_swatch" => plain(viewer.detach_swatch(layer()?, a.row_kind("kind")?)),
        // ---- design ----
        "grid_append_child_at" => unit(viewer.grid_append_child_at(a.text("container")?, a.text("node")?, a.parsed("row", "row")?, a.parsed("column", "column")?)),
        "set_grid_gaps" => unit(viewer.set_grid_gaps(a.text("node")?, a.number("row_gap")?, a.number("column_gap")?)),
        "auto_layout_control" => unit(viewer.auto_layout_control(a.text("widget_id")?, a.text("value")?)),
        // ---- shell state and proof ----
        "help_search" => match viewer.help_search(a.text("text")?) {
            Ok(rows) => Ok(rows.into()),
            Err(code) => return Err(refuse(code, Value::Null)),
        },
        "help_close" => {
            viewer.help_close();
            Ok(Value::Null)
        }
        "dialog_accept" => plain(viewer.dialog_accept()),
        "dialog_cancel" => {
            viewer.dialog_cancel();
            Ok(Value::Null)
        }
        "close" => {
            viewer.close();
            Ok(Value::Null)
        }
        "proof_apply_draft" => unit(viewer.proof_apply_draft()),
        "proof_close_page" => {
            viewer.proof_close_page();
            Ok(Value::Null)
        }
        "proof_set_save_name" => {
            viewer.proof_set_save_name(a.text("name")?);
            Ok(Value::Null)
        }
        other => return Err(refuse("command_not_driveable", serde_json::json!({"command_id": format!("viewer.{other}")}))),
    })
}
