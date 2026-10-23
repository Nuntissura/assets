//! Lane SMB-HARNESS-PANELS-1 (feature `viewer`): every surface the GUI audit found unreachable from
//! the window is hosted behind WORKSPACE > Panels and proven per surface, headlessly (no window,
//! CX-SAFE-002; STU-QUIET-005 state/AccessKit fallback):
//! 1. its keyboard shortcut (a real egui key event) and its WORKSPACE command
//!    (`Viewer::shell_command`, the call the menu entry makes) open, hide and show it;
//! 2. one UI frame draws it (its stable widget names);
//! 3. its key state is in `Viewer::state_json()["shell"]` (`hosted.<key>` / `surfaces.<key>`) and,
//!    with feature `desktop`, in the AccessKit tree (the summary label equals the projected summary;
//!    a control clicked through an AccessKit action request drives the existing Viewer method);
//! 4. opening it emits no focus-moving viewport command and leaves egui keyboard focus untouched.
//!
//! Oracles: the Viewer's own model results (paint style rows, bounds, layer counts, records),
//! files written to a scratch dir, and the state projection.
#![cfg(feature = "viewer")]

mod common;
mod support_audio;

use common::{Scratch, solid_psd};
use hsk_studio_accord::{CancellationToken, Ticks};
use hsk_studio_folio::{self as folio, *};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::hosted::{self, BrushChoice, HostedPanel, PanelAction};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::shell::{Panel, ShellCommand};
use hsk_studio_harness::view::vector::arrange::{Align, ReferencePoint};
use hsk_studio_harness::view::vector::{AlignTarget, EnvelopeSpec, PaintSpec, PathEdit, RowKind, ShapeKind, StrokePanel, TransformOp, VectorTool};
use serde_json::Value;
use std::path::{Path, PathBuf};

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];

// ---- helpers ---------------------------------------------------------------------------------------

fn input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))), events, ..Default::default() }
}

fn frame(v: &mut Viewer, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    ctx.run(input(events), |ctx| v.ui(ctx))
}

fn key(k: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers }
}

fn drew(v: &Viewer, name: &str) -> bool {
    v.drawn().iter().any(|d| d == name)
}

fn shell(v: &Viewer) -> Value {
    v.state_json()["shell"].clone()
}

fn hosted_state(v: &Viewer, h: HostedPanel) -> Value {
    shell(v)["hosted"][h.key()].clone()
}

fn ctx() -> egui::Context {
    let ctx = egui::Context::default();
    #[cfg(feature = "desktop")]
    ctx.enable_accesskit();
    ctx
}

/// No viewport command that moves OS focus, raises, shows or warps anything; egui keyboard focus is
/// where it was before the surface opened (`before`), never taken by it (STU-QUIET-005, STU-SHL-009).
fn quiet(ctx: &egui::Context, out: &egui::FullOutput, before: Option<egui::Id>) {
    for vo in out.viewport_output.values() {
        for c in &vo.commands {
            let stealing = matches!(
                c,
                egui::ViewportCommand::Focus
                    | egui::ViewportCommand::RequestUserAttention(_)
                    | egui::ViewportCommand::Visible(_)
                    | egui::ViewportCommand::Minimized(_)
                    | egui::ViewportCommand::Maximized(_)
                    | egui::ViewportCommand::Fullscreen(_)
                    | egui::ViewportCommand::OuterPosition(_)
                    | egui::ViewportCommand::CursorPosition(_)
                    | egui::ViewportCommand::CursorGrab(_)
            );
            assert!(!stealing, "focus-moving viewport command {c:?}");
        }
    }
    assert_eq!(ctx.memory(|m| m.focused()), before, "a hosted surface took keyboard focus");
}

#[cfg(feature = "desktop")]
fn ak_node(out: &egui::FullOutput, label: &str) -> Option<(egui::accesskit::NodeId, bool)> {
    let tree = out.platform_output.accesskit_update.as_ref()?;
    // egui puts a button's text in the node label and a plain label's text in the node value.
    tree.nodes.iter().find(|(_, n)| n.label().is_some_and(|l| l == label) || n.value().is_some_and(|l| l == label)).map(|(id, n)| (*id, n.is_disabled()))
}

/// Clicks the enabled control labelled `label` through an AccessKit action request (egui's own
/// input path) and draws the frame that runs it.
#[cfg(feature = "desktop")]
fn ak_click(v: &mut Viewer, ctx: &egui::Context, label: &str) {
    let out = frame(v, ctx, vec![]);
    let (id, disabled) = ak_node(&out, label).unwrap_or_else(|| panic!("no AccessKit node '{label}': {:?}", v.drawn()));
    assert!(!disabled, "'{label}' is disabled");
    let click = egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest { action: egui::accesskit::Action::Click, target: id, data: None });
    let _ = frame(v, ctx, vec![click]);
}

/// Opens hosted panel `h` with its shortcut, proves drawing, state, AccessKit summary and quiet;
/// hides and shows it through its WORKSPACE command.
fn open_hosted(v: &mut Viewer, ctx: &egui::Context, h: HostedPanel) {
    let p = Panel::Hosted(h);
    assert_eq!(shell(v)["panels"][h.key()]["availability"], "available", "{}", shell(v)["panels"][h.key()]);
    assert_eq!(shell(v)["panels"][h.key()]["shortcut"], h.shortcut_text());
    let before = ctx.memory(|m| m.focused());
    let _ = frame(v, ctx, vec![key(h.key_code(), h.modifiers())]);
    assert_eq!(hosted_state(v, h)["open"], true, "{h:?}: {}", v.state_json()["status"]);
    assert_eq!(hosted_state(v, h)["visible"], true);
    let out = frame(v, ctx, vec![]);
    assert!(drew(v, &hosted::widget::window(h.key())) && drew(v, &hosted::widget::summary(h.key())), "{h:?} not drawn: {:?}", v.drawn());
    quiet(ctx, &out, before);
    let summary = hosted_state(v, h)["summary"].as_str().unwrap().to_owned();
    assert_eq!(summary, v.hosted_summary(h));
    #[cfg(feature = "desktop")]
    assert!(ak_node(&out, &summary).is_some(), "{h:?}: summary '{summary}' not in the AccessKit tree");
    v.shell_command(ShellCommand::Panel(p)).unwrap();
    let _ = frame(v, ctx, vec![]);
    assert!(!drew(v, &hosted::widget::window(h.key())));
    assert_eq!(hosted_state(v, h)["visible"], false);
    v.shell_command(ShellCommand::Panel(p)).unwrap();
    let out = frame(v, ctx, vec![]);
    assert!(drew(v, &hosted::widget::window(h.key())));
    quiet(ctx, &out, before);
}

/// A 96 x 64 white document with a red rectangle (selected) and a blue one.
fn paper(scratch: &Scratch) -> (Viewer, String, String) {
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(96, 64, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: true, fill: RED, stroke_on: false, stroke: BLUE, stroke_width: 2.0, ..VectorTool::default() });
    let a = v.draw_shape(ShapeKind::Rectangle, [5.0, 5.0], [30.0, 30.0], false).unwrap();
    let b = v.draw_shape(ShapeKind::Rectangle, [50.0, 20.0], [90.0, 40.0], false).unwrap();
    v.select(Some(a.clone()));
    (v, a, b)
}

fn rows(v: &Viewer, layer: &str) -> (usize, usize) {
    let s = v.layer_paint_style(layer).unwrap();
    let fills = s.rows.iter().filter(|r| matches!(r, StudioAppearanceRow::Fill { .. })).count();
    (fills, s.rows.len() - fills)
}

/// RGB of a pixel of the 96 x 64 paper composite.
fn pixel(v: &mut Viewer, x: usize, y: usize) -> [u8; 3] {
    v.render_if_dirty();
    let f = &v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).rgba8;
    let i = (y * 96 + x) * 4;
    [f[i], f[i + 1], f[i + 2]]
}

fn layer_count(v: &Viewer) -> usize {
    v.state_json()["layers"].as_array().unwrap().len()
}

// ---- vector panels ---------------------------------------------------------------------------------

#[test]
fn appearance_panel_sets_and_clears_the_fill_of_the_selected_layer() {
    let scratch = Scratch::new("panels-appearance");
    let (mut v, a, _) = paper(&scratch);
    let ctx = ctx();
    open_hosted(&mut v, &ctx, HostedPanel::Appearance);
    v.hosted_action(PanelAction::SetFill(Some(GREEN))).unwrap();
    let style = v.layer_paint_style(&a).unwrap();
    assert!(style.rows.iter().any(|r| matches!(r, StudioAppearanceRow::Fill { paint: StudioPaint::Solid { colour }, .. } if (colour.g - 1.0).abs() < 1e-9 && colour.r.abs() < 1e-9)), "{style:?}");
    assert_eq!(rows(&v, &a).0, 1);
    #[cfg(feature = "desktop")]
    ak_click(&mut v, &ctx, "No Fill");
    #[cfg(not(feature = "desktop"))]
    v.hosted_action(PanelAction::SetFill(None)).unwrap();
    assert_eq!(rows(&v, &a).0, 0, "No Fill removed the fill row");
    assert!(hosted_state(&v, HostedPanel::Appearance)["summary"].as_str().unwrap().ends_with("0 fill rows, 0 stroke rows"), "{}", hosted_state(&v, HostedPanel::Appearance));
    assert!(hosted_state(&v, HostedPanel::Appearance)["last"].as_str().unwrap().contains("fill set"));
    // One undo step back to the green fill.
    v.undo().unwrap();
    assert_eq!(rows(&v, &a).0, 1);
    // Without a selection the action is refused, typed, and nothing changes.
    v.select(None);
    let ops = v.run().ops().len();
    assert!(v.hosted_action(PanelAction::ClearAppearance).unwrap_err().contains("select a layer"));
    assert_eq!(v.run().ops().len(), ops);
}

#[test]
fn stroke_panel_applies_and_removes_a_stroke() {
    let scratch = Scratch::new("panels-stroke");
    let (mut v, a, _) = paper(&scratch);
    let ctx = ctx();
    open_hosted(&mut v, &ctx, HostedPanel::Stroke);
    let panel = StrokePanel { cap: folio::StrokeCap::Round, join: folio::StrokeJoin::Bevel, dash: vec![4.0, 2.0], ..StrokePanel::solid(BLUE, 3.0) };
    v.hosted_action(PanelAction::SetStroke(Some(panel))).unwrap();
    assert_eq!(rows(&v, &a), (1, 1));
    #[cfg(feature = "desktop")]
    ak_click(&mut v, &ctx, "No Stroke");
    #[cfg(not(feature = "desktop"))]
    v.hosted_action(PanelAction::SetStroke(None)).unwrap();
    assert_eq!(rows(&v, &a), (1, 0));
    #[cfg(feature = "desktop")]
    {
        // Apply Stroke with the panel's defaults (black, weight 1, solid) through its button.
        ak_click(&mut v, &ctx, "Apply Stroke");
        assert_eq!(rows(&v, &a), (1, 1));
    }
}

#[test]
fn swatches_panel_adds_a_swatch_and_links_it_to_the_fill() {
    let scratch = Scratch::new("panels-swatches");
    let (mut v, a, _) = paper(&scratch);
    let ctx = ctx();
    open_hosted(&mut v, &ctx, HostedPanel::Swatches);
    #[cfg(feature = "desktop")]
    ak_click(&mut v, &ctx, "New Swatch");
    #[cfg(not(feature = "desktop"))]
    v.hosted_action(PanelAction::AddSwatch { name: "Swatch".into(), colour: BLUE }).unwrap();
    let swatches = v.swatches().unwrap();
    assert_eq!(swatches.len(), 1, "{}", v.state_json()["status"]);
    assert_eq!(hosted_state(&v, HostedPanel::Swatches)["swatches"][0][0], swatches[0].0.as_str());
    assert_eq!(hosted_state(&v, HostedPanel::Swatches)["summary"], "1 swatches");
    #[cfg(feature = "desktop")]
    ak_click(&mut v, &ctx, "To Fill");
    #[cfg(not(feature = "desktop"))]
    v.hosted_action(PanelAction::ApplySwatch { swatch_id: swatches[0].0.clone(), kind: RowKind::Fill, tint: None }).unwrap();
    assert!(hosted_state(&v, HostedPanel::Swatches)["last"].as_str().unwrap().contains("fill linked to"), "{}", hosted_state(&v, HostedPanel::Swatches));
    let c = v.swatch_colour(&swatches[0].0).unwrap();
    assert!((c[2] - 1.0).abs() < 1e-6 && c[0].abs() < 1e-6, "{c:?}");
    v.hosted_action(PanelAction::DetachSwatch(RowKind::Fill)).unwrap();
    assert_eq!(rows(&v, &a).0, 1, "detaching keeps a copy of the colour");
}

#[test]
fn graphic_styles_panel_creates_a_style_and_applies_it() {
    let scratch = Scratch::new("panels-styles");
    let (mut v, a, b) = paper(&scratch);
    let ctx = ctx();
    v.set_fill(&a, Some(&PaintSpec::Solid(GREEN))).unwrap();
    open_hosted(&mut v, &ctx, HostedPanel::GraphicStyles);
    #[cfg(feature = "desktop")]
    ak_click(&mut v, &ctx, "New from Selection");
    #[cfg(not(feature = "desktop"))]
    v.hosted_action(PanelAction::AddGraphicStyle { name: "Style".into() }).unwrap();
    let styles = v.graphic_styles().unwrap();
    assert_eq!(styles.len(), 1, "{}", v.state_json()["status"]);
    assert_eq!(hosted_state(&v, HostedPanel::GraphicStyles)["summary"], "1 graphic styles");
    v.select(Some(b.clone()));
    #[cfg(feature = "desktop")]
    ak_click(&mut v, &ctx, "Apply");
    #[cfg(not(feature = "desktop"))]
    v.hosted_action(PanelAction::ApplyGraphicStyle(styles[0].0.clone())).unwrap();
    assert_eq!(pixel(&mut v, 70, 30), GREEN, "b (red before) wears a's green appearance");
    v.hosted_action(PanelAction::BreakGraphicStyle).unwrap();
}

#[test]
fn align_and_transform_panels_move_their_target_layers() {
    let scratch = Scratch::new("panels-align");
    let (mut v, a, b) = paper(&scratch);
    let ctx = ctx();
    open_hosted(&mut v, &ctx, HostedPanel::AlignDistribute);
    #[cfg(feature = "desktop")]
    {
        ak_click(&mut v, &ctx, "All");
        assert_eq!(hosted_state(&v, HostedPanel::AlignDistribute)["targets"].as_array().unwrap().len(), 2);
        assert_eq!(hosted_state(&v, HostedPanel::AlignDistribute)["summary"], "2 target layers");
        ak_click(&mut v, &ctx, "Left");
    }
    #[cfg(not(feature = "desktop"))]
    v.hosted_action(PanelAction::Align { layers: vec![a.clone(), b.clone()], to: AlignTarget::Selection, how: Align::Left }).unwrap();
    let (ba, bb) = (v.layer_bounds(&a).unwrap(), v.layer_bounds(&b).unwrap());
    assert!((ba.min[0] - 5.0).abs() < 1e-6 && (bb.min[0] - 5.0).abs() < 1e-6, "{ba:?} {bb:?}");
    // Align to the key object (the selected layer a, top 5): b's top moves to 5.
    v.hosted_action(PanelAction::Align { layers: vec![a.clone(), b.clone()], to: AlignTarget::KeyObject(a.clone()), how: Align::Top }).unwrap();
    assert!((v.layer_bounds(&b).unwrap().min[1] - 5.0).abs() < 1e-6, "{:?}", v.layer_bounds(&b));
    // Transform: Move by (10, 0) about the centre, one step for both layers.
    open_hosted(&mut v, &ctx, HostedPanel::Transform);
    v.hosted_action(PanelAction::Transform { layers: vec![a.clone(), b.clone()], op: TransformOp::Move { dx: 10.0, dy: 0.0 }, reference: ReferencePoint::Centre }).unwrap();
    assert!((v.layer_bounds(&a).unwrap().min[0] - 15.0).abs() < 1e-6 && (v.layer_bounds(&b).unwrap().min[0] - 15.0).abs() < 1e-6);
    v.undo().unwrap();
    assert!((v.layer_bounds(&b).unwrap().min[0] - 5.0).abs() < 1e-6, "one undo step moves both back");
}

#[test]
fn path_operations_panel_offsets_reverses_and_rounds_a_corner() {
    let scratch = Scratch::new("panels-path");
    let (mut v, a, _) = paper(&scratch);
    let ctx = ctx();
    open_hosted(&mut v, &ctx, HostedPanel::PathOperations);
    assert!(hosted_state(&v, HostedPanel::PathOperations)["summary"].as_str().unwrap().ends_with(": live shape"), "{}", hosted_state(&v, HostedPanel::PathOperations));
    let before = layer_count(&v);
    #[cfg(feature = "desktop")]
    ak_click(&mut v, &ctx, "Offset Path");
    #[cfg(not(feature = "desktop"))]
    v.hosted_action(PanelAction::OffsetPath { offset: 2.0, join: hsk_studio_nib::stroke::LineJoin::Miter }).unwrap();
    assert_eq!(layer_count(&v), before + 1, "offset path adds a layer: {}", v.state_json()["status"]);
    v.select(Some(a.clone()));
    v.hosted_action(PanelAction::ExpandShape).unwrap();
    assert!(hosted_state(&v, HostedPanel::PathOperations)["summary"].as_str().unwrap().ends_with(": path"));
    v.hosted_action(PanelAction::EditPath(PathEdit::Reverse(None))).unwrap();
    let corner = PathEdit::Corner { at: hsk_studio_nib::anchor_path::AnchorAddr { contour: 0, anchor: 0 }, kind: hsk_studio_nib::freeform::CornerTreatment::Round, radius: 4.0 };
    v.hosted_action(PanelAction::EditPath(corner)).unwrap();
    // A hostile anchor address is a typed refusal, nothing committed.
    let ops = v.run().ops().len();
    assert!(v.hosted_action(PanelAction::EditPath(PathEdit::SplitAtAnchor(hsk_studio_nib::anchor_path::AnchorAddr { contour: 99, anchor: 99 }))).is_err());
    assert_eq!(v.run().ops().len(), ops);
}

#[test]
fn masks_panel_clips_and_releases() {
    let scratch = Scratch::new("panels-masks");
    let (mut v, a, b) = paper(&scratch);
    let ctx = ctx();
    open_hosted(&mut v, &ctx, HostedPanel::Masks);
    v.hosted_action(PanelAction::MakeClip { source: b.clone(), even_odd: false }).unwrap();
    assert!(hosted_state(&v, HostedPanel::Masks)["summary"].as_str().unwrap().ends_with(": clipped"), "{}", hosted_state(&v, HostedPanel::Masks));
    #[cfg(feature = "desktop")]
    ak_click(&mut v, &ctx, "Release");
    #[cfg(not(feature = "desktop"))]
    v.hosted_action(PanelAction::ReleaseMasks).unwrap();
    assert!(hosted_state(&v, HostedPanel::Masks)["summary"].as_str().unwrap().ends_with(": no clip or mask"));
    v.hosted_action(PanelAction::MakeMask { source: b.clone(), mode: StudioMaskMode::Alpha, invert: true }).unwrap();
    assert!(hosted_state(&v, HostedPanel::Masks)["summary"].as_str().unwrap().ends_with(": masked"));
    assert!(v.hosted_action(PanelAction::MakeClip { source: a, even_odd: false }).is_err(), "a layer cannot clip itself");
}

#[test]
fn envelope_panel_makes_releases_and_expands() {
    let scratch = Scratch::new("panels-envelope");
    let (mut v, _, _) = paper(&scratch);
    let ctx = ctx();
    open_hosted(&mut v, &ctx, HostedPanel::EnvelopeDistort);
    #[cfg(feature = "desktop")]
    ak_click(&mut v, &ctx, "Make");
    #[cfg(not(feature = "desktop"))]
    v.hosted_action(PanelAction::MakeEnvelope { spec: EnvelopeSpec::Warp { style: StudioWarpStyle::Arc, axis: StudioWarpAxis::Horizontal, bend: 50.0, horizontal: 0.0, vertical: 0.0 }, fidelity: 50.0, distort_appearance: false }).unwrap();
    assert!(hosted_state(&v, HostedPanel::EnvelopeDistort)["summary"].as_str().unwrap().ends_with(": warp envelope"), "{}", v.state_json()["status"]);
    #[cfg(feature = "desktop")]
    ak_click(&mut v, &ctx, "Release");
    #[cfg(not(feature = "desktop"))]
    v.hosted_action(PanelAction::ReleaseEnvelope).unwrap();
    assert!(hosted_state(&v, HostedPanel::EnvelopeDistort)["summary"].as_str().unwrap().ends_with(": no envelope"));
    v.hosted_action(PanelAction::MakeEnvelope { spec: EnvelopeSpec::Mesh { rows: 2, columns: 2 }, fidelity: 50.0, distort_appearance: false }).unwrap();
    assert!(hosted_state(&v, HostedPanel::EnvelopeDistort)["summary"].as_str().unwrap().ends_with(": mesh envelope"));
    v.hosted_action(PanelAction::ExpandEnvelope).unwrap();
    assert!(hosted_state(&v, HostedPanel::EnvelopeDistort)["summary"].as_str().unwrap().ends_with(": no envelope"));
    assert!(v.hosted_action(PanelAction::MakeEnvelope { spec: EnvelopeSpec::Mesh { rows: 0, columns: 2 }, fidelity: 50.0, distort_appearance: false }).is_err());
}

#[test]
fn brushes_panel_creates_applies_and_removes_a_brush() {
    let scratch = Scratch::new("panels-brushes");
    let (mut v, a, _) = paper(&scratch);
    let ctx = ctx();
    v.set_vector_tool(VectorTool { fill_on: false, stroke_on: true, stroke: BLUE, stroke_width: 2.0, ..v.vector_tool().clone() });
    let line = v.draw_shape(ShapeKind::Line, [10.0, 50.0], [90.0, 50.0], false).unwrap();
    open_hosted(&mut v, &ctx, HostedPanel::Brushes);
    v.hosted_action(PanelAction::AddBrush { name: "Leaf".into(), kind: BrushChoice::Art, colorization: StudioBrushColorization::None, layers: vec![a.clone()] }).unwrap();
    let brushes = v.brushes().unwrap();
    assert_eq!(brushes.len(), 1);
    assert!(hosted_state(&v, HostedPanel::Brushes)["summary"].as_str().unwrap().starts_with("1 brushes"));
    v.select(Some(line.clone()));
    #[cfg(feature = "desktop")]
    ak_click(&mut v, &ctx, "Apply");
    #[cfg(not(feature = "desktop"))]
    v.hosted_action(PanelAction::ApplyBrush { brush_id: brushes[0].0.clone(), scale: None }).unwrap();
    assert!(hosted_state(&v, HostedPanel::Brushes)["last"].as_str().unwrap().contains("brushed with"), "{}", hosted_state(&v, HostedPanel::Brushes));
    #[cfg(feature = "desktop")]
    ak_click(&mut v, &ctx, "Remove Brush");
    #[cfg(not(feature = "desktop"))]
    v.hosted_action(PanelAction::RemoveBrush).unwrap();
    assert!(hosted_state(&v, HostedPanel::Brushes)["last"].as_str().unwrap().contains("brush removed"));
    assert!(v.hosted_action(PanelAction::ApplyBrush { brush_id: "nope".into(), scale: None }).is_err());
}

#[test]
fn vector_export_panel_writes_svg_and_pdf_and_never_overwrites() {
    let scratch = Scratch::new("panels-export");
    let (mut v, _, _) = paper(&scratch);
    let ctx = ctx();
    open_hosted(&mut v, &ctx, HostedPanel::VectorExport);
    assert_eq!(hosted_state(&v, HostedPanel::VectorExport)["summary"], "no export path");
    #[cfg(feature = "desktop")]
    {
        // Without a path both export buttons are disabled nodes.
        let out = frame(&mut v, &ctx, vec![]);
        assert_eq!(ak_node(&out, "Export SVG").map(|n| n.1), Some(true));
        assert_eq!(ak_node(&out, "Export PDF").map(|n| n.1), Some(true));
    }
    let base = scratch.0.join("art");
    v.hosted_action(PanelAction::ExportSvg { path: base.with_extension("svg"), overwrite: false }).unwrap();
    v.hosted_action(PanelAction::ExportPdf { path: base.with_extension("pdf"), overwrite: false }).unwrap();
    let svg = std::fs::read_to_string(base.with_extension("svg")).unwrap();
    assert!(svg.contains("<svg") && svg.contains("<path"), "{svg}");
    assert!(std::fs::read(base.with_extension("pdf")).unwrap().starts_with(b"%PDF-"));
    assert!(v.hosted_action(PanelAction::ExportSvg { path: base.with_extension("svg"), overwrite: false }).unwrap_err().contains("exists"));
    assert!(hosted_state(&v, HostedPanel::VectorExport)["last"].as_str().unwrap().contains("exists"));
}

/// A PDF-compatible vector artwork file with one red rectangle (the private-data marker in the
/// info dictionary, as `viewer_ai_interop.rs` builds it).
fn artwork() -> Vec<u8> {
    use hsk_studio_press::*;
    let options = ExportOptions { info_title: Some("AIPrivateData Illustrator fixture"), ..ExportOptions::default() };
    let mut ex = PdfExport::new(options, &Limits::default()).unwrap();
    let p = ex.add_page(PageSpec { media_box_pt: [0.0, 0.0, 150.0, 75.0], trim_box_pt: None, bleed_box_pt: None }).unwrap();
    let rect = vec![PathCommand::MoveTo(6.0, 6.0), PathCommand::LineTo(30.0, 6.0), PathCommand::LineTo(30.0, 24.0), PathCommand::LineTo(6.0, 24.0), PathCommand::Close];
    ex.fill_path(p, &rect, FillRule::NonZero, Colour::DeviceRgb([1.0, 0.0, 0.0])).unwrap();
    ex.finish(&CancellationToken::default()).unwrap().0
}

#[test]
fn vector_import_panel_opens_artwork_as_editable_vectors() {
    let scratch = Scratch::new("panels-import");
    let mut v = Viewer::new(Prefer::Cpu);
    let ctx = ctx();
    // Import needs no open document.
    open_hosted(&mut v, &ctx, HostedPanel::VectorImport);
    #[cfg(feature = "desktop")]
    {
        let out = frame(&mut v, &ctx, vec![]);
        assert_eq!(ak_node(&out, "Import as Editable Vectors").map(|n| n.1), Some(true), "disabled until both paths are typed");
    }
    let src = scratch.0.join("art.ai");
    std::fs::write(&src, artwork()).unwrap();
    let dest = scratch.0.join("art.handshake");
    v.hosted_action(PanelAction::ImportVector { source: src.clone(), dest: dest.clone(), overwrite: false }).unwrap();
    assert!(dest.is_file());
    assert!(v.state_json()["layers"].as_array().unwrap().iter().any(|l| l["kind"] == "vector"), "{}", v.state_json()["layers"]);
    assert!(v.hosted_action(PanelAction::ImportVector { source: src, dest, overwrite: false }).unwrap_err().contains("exists"));
    assert!(v.hosted_action(PanelAction::ImportVector { source: scratch.0.join("missing.ai"), dest: scratch.0.join("x.handshake"), overwrite: false }).is_err());
}

#[test]
fn audio_batch_panel_runs_the_queue_or_names_its_feature() {
    let scratch = Scratch::new("panels-batch");
    let mut v = Viewer::new(Prefer::Cpu);
    let ctx = ctx();
    #[cfg(feature = "audio-batch")]
    {
        use hsk_studio_harness::view::audio_import::MixFormat;
        use hsk_studio_harness::view::hosted::BatchItem;
        open_hosted(&mut v, &ctx, HostedPanel::AudioBatch);
        let wav = scratch.0.join("tone.wav");
        std::fs::write(&wav, support_audio::wav(2, 48_000, support_audio::WavFormat::Float32, None, &support_audio::signal(4800, 2))).unwrap();
        let out = scratch.0.join("tone.flac");
        v.hosted_action(PanelAction::BatchAdd(BatchItem { input: wav.clone(), output: out.clone(), gain_db: Some(-6.0), format: MixFormat::Flac { bits: 24 }, overwrite: false })).unwrap();
        v.hosted_action(PanelAction::BatchAdd(BatchItem { input: scratch.0.join("missing.wav"), output: scratch.0.join("missing.flac"), gain_db: None, format: MixFormat::Flac { bits: 16 }, overwrite: false })).unwrap();
        assert_eq!(hosted_state(&v, HostedPanel::AudioBatch)["summary"], "2 queued, 0 receipts");
        #[cfg(feature = "desktop")]
        ak_click(&mut v, &ctx, "Run Queue");
        #[cfg(not(feature = "desktop"))]
        v.hosted_action(PanelAction::BatchRun).unwrap();
        let s = hosted_state(&v, HostedPanel::AudioBatch);
        assert_eq!(s["summary"], "0 queued, 2 receipts", "{s}");
        assert_eq!(s["receipts"][0]["outcome"], "complete", "{s}");
        assert_ne!(s["receipts"][1]["outcome"], "complete", "{s}");
        assert!(out.is_file());
        assert!(s["last"].as_str().unwrap().contains("1 of 2 complete"));
    }
    #[cfg(not(feature = "audio-batch"))]
    {
        let _ = (&scratch, &ctx);
        let p = &shell(&v)["panels"]["audio_batch"];
        assert_eq!((p["availability"].as_str(), p["feature"].as_str()), (Some("needs_feature"), Some("audio-batch")));
        assert!(v.toggle_panel(Panel::Hosted(HostedPanel::AudioBatch)).unwrap_err().contains("needs feature audio-batch"));
    }
}

#[test]
fn every_hosted_panel_has_a_unique_key_shortcut_and_needs_a_document_when_it_edits_one() {
    let mut keys: Vec<&str> = HostedPanel::ALL.iter().map(|h| h.key()).collect();
    let mut shortcuts: Vec<(egui::Key, bool, bool)> = HostedPanel::ALL.iter().map(|h| (h.key_code(), h.modifiers().shift, h.modifiers().command)).collect();
    // The shell's own panels use plain function keys.
    shortcuts.extend(Panel::ALL.iter().filter(|p| !matches!(p, Panel::Hosted(_))).map(|p| (p.key_code(), false, false)));
    keys.extend(Panel::ALL.iter().filter(|p| !matches!(p, Panel::Hosted(_))).map(|p| p.key()));
    let (nk, ns) = (keys.len(), shortcuts.len());
    keys.sort();
    keys.dedup();
    shortcuts.sort_by_key(|s| format!("{s:?}"));
    shortcuts.dedup();
    assert_eq!((keys.len(), shortcuts.len()), (nk, ns), "duplicate panel key or shortcut");
    let v = Viewer::new(Prefer::Cpu);
    for h in HostedPanel::ALL {
        let p = &shell(&v)["panels"][h.key()];
        assert_eq!(p["command"], format!("studio.workspace.panel.{}", h.key()));
        if h.feature().is_some_and(|f| !hsk_studio_harness::view::shell::CompiledFeatures::current().has(f)) {
            continue;
        }
        let want = if h.needs_document() { "unavailable" } else { "available" };
        assert_eq!(p["availability"], want, "{h:?}: {p}");
    }
    // No vendor product name in any hosted label or key (STU-SHL-008).
    for h in HostedPanel::ALL {
        for vendor in ["Adobe", "Illustrator", "Photoshop", "InDesign", "Premiere", "After Effects", "Audition", "Figma", "Affinity", "Lightroom"] {
            assert!(!h.label().contains(vendor) && !h.key().contains(&vendor.to_lowercase()), "{h:?} names {vendor}");
        }
    }
}

// ---- shell-hosted surfaces (lane SMB-HARNESS-SHELL-1) -----------------------------------------------

/// A native package whose document has one sequence (`seq-main`, one audio track with one clip).
fn sequence_package(dir: &Path) -> PathBuf {
    const F: u64 = 10_160_640_000;
    let clip = StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: "SCLP-019abcde-0000-7000-8000-000000000001".into(),
        source_ref: ClipSource::Media { artifact_manifest_id: "tone".into(), content_digest: ContentDigest { algorithm: "sha256".into(), digest: "abc123".into() }, asset_id: None },
        source_in: Ticks::new(0),
        source_out: Ticks::new(25 * F),
        timeline_start: Ticks::new(0),
        duration: Ticks::new(25 * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: ClipTransitions::default(),
    };
    let track = StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: "STRK-019abcde-0000-7000-8000-000000000001".into(),
        kind: TrackKind::Audio,
        index: 0,
        name: "A1".into(),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted: true,
        muted: false,
        soloed: false,
        clips: vec![clip],
        volume: 1.0,
        pan: 0.0,
    };
    let sequence = StudioSequence {
        schema_id: SequenceSchema::V1,
        sequence_id: "seq-main".into(),
        name: "main".into(),
        frame_rate: SequenceFrameRate { ticks_per_frame: F },
        duration: Ticks::new(25 * F),
        tracks: vec![track],
        markers: Vec::new(),
        work_area: None,
    };
    let doc = serde_json::json!({
        "schema_id":"hsk.studio.document@1","document_id":"SDOC-019abcde-0000-7000-8000-000000000003","revision":1,"geometry_unit":"px","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[{"schema_id":"hsk.studio.layer@1","layer_id":"SLYR-019abcde-0000-7000-8000-000000000001","name":"group","parent":null,"kind":"group","payload":{"payload_kind":"group"},"visible":true}],
        "graph":{"schema_id":"hsk.studio.layer_graph@1","operations":[],"edges":[],"outputs":[]},
        "sequences":[serde_json::to_value(&sequence).unwrap()]
    });
    struct Any;
    impl folio::Resolver for Any {
        fn primitive(&self, _: &str, _: &str) -> Resolution {
            Resolution::Available
        }
        fn profile(&self, _: &str) -> Resolution {
            Resolution::Available
        }
        fn tile(&self, _: &TileRef) -> Resolution {
            Resolution::Available
        }
        fn format_supported(&self, _: &str) -> bool {
            true
        }
        fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
            false
        }
        fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
            Resolution::Available
        }
    }
    let token = CancellationToken::default();
    let snapshot = match folio::inspect_bytes(&serde_json::to_vec(&doc).unwrap(), Budget::default(), &token, &Any).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let assets = hsk_studio_package::MemoryAssetSource::new();
    let mut bytes = Vec::new();
    hsk_studio_package::write_package_with(
        &snapshot,
        &assets as &dyn hsk_studio_package::AssetSource,
        &hsk_studio_package::Extras { quarantined: &[], previews: &[] },
        &hsk_studio_package::WriteOptions::default(),
        &hsk_studio_package::Limits::default(),
        &token,
        &mut bytes,
    )
    .unwrap();
    let path = dir.join("sequence.handshake");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Opens a shell-hosted surface with its plain function key, proves the frame, quiet and that the
/// panel entry reports it visible.
fn open_surface(v: &mut Viewer, ctx: &egui::Context, p: Panel) -> egui::FullOutput {
    let before = ctx.memory(|m| m.focused());
    let _ = frame(v, ctx, vec![key(p.key_code(), egui::Modifiers::NONE)]);
    let out = frame(v, ctx, vec![]);
    quiet(ctx, &out, before);
    out
}

#[test]
fn layout_editor_surface_is_reachable_and_projects_its_state() {
    let mut v = Viewer::new(Prefer::Cpu);
    let ctx = ctx();
    let _out = open_surface(&mut v, &ctx, Panel::LayoutEditor);
    assert_eq!(shell(&v)["panels"]["layout_editor"]["visible"], true);
    assert!(drew(&v, "layout") && drew(&v, "layout.pages"), "{:?}", v.drawn());
    let s = &shell(&v)["surfaces"]["layout_editor"];
    assert!(s.is_object(), "layout editor state projected: {s}");
    #[cfg(feature = "desktop")]
    assert!(ak_node(&_out, "Text frame").is_some() && ak_node(&_out, "Thread").is_some(), "layout editor tools in the AccessKit tree");
    // Hidden by the WORKSPACE command: the editor (and its state) stays.
    v.shell_command(ShellCommand::Panel(Panel::LayoutEditor)).unwrap();
    let _ = frame(&mut v, &ctx, vec![]);
    assert!(!drew(&v, "layout"));
    assert!(shell(&v)["surfaces"]["layout_editor"].is_object());
}

#[test]
fn program_monitor_surface_is_reachable_and_projects_its_transport() {
    let scratch = Scratch::new("panels-monitor");
    let mut v = Viewer::new(Prefer::Cpu);
    let ctx = ctx();
    v.open(&sequence_package(&scratch.0)).unwrap();
    assert!(shell(&v)["surfaces"]["program_monitor"].is_null());
    let _out = open_surface(&mut v, &ctx, Panel::ProgramMonitor);
    assert!(drew(&v, "monitor") && drew(&v, "monitor.play"), "{:?}", v.drawn());
    let m = shell(&v)["surfaces"]["program_monitor"].clone();
    assert!(m.is_object(), "{m}");
    assert_eq!(m, v.monitor().unwrap().state_json());
    #[cfg(feature = "desktop")]
    {
        ak_click(&mut v, &ctx, "Play");
        assert_ne!(shell(&v)["surfaces"]["program_monitor"], m, "Play changed the projected transport");
    }
}

#[test]
fn prototype_player_surface_is_reachable_and_projects_its_state() {
    let scratch = Scratch::new("panels-prototype");
    let mut v = Viewer::new(Prefer::Cpu);
    let ctx = ctx();
    let fig = scratch.0.join("proto.fig");
    std::fs::copy(format!("{}/../hsk-studio-interop-fig/tests/fixtures/fig-prototype-pages.fig", env!("CARGO_MANIFEST_DIR")), &fig).unwrap();
    let _ = open_surface(&mut v, &ctx, Panel::PrototypePlayer);
    assert_eq!(shell(&v)["dialog"]["purpose"], "open_prototype");
    v.file_dialog().unwrap().typed = fig.to_string_lossy().into_owned();
    assert_eq!(v.dialog_accept(), Ok(true));
    let out = frame(&mut v, &ctx, vec![]);
    quiet(&ctx, &out, None);
    assert!(drew(&v, hsk_studio_harness::view::prototype::widget::PANEL), "{:?}", v.drawn());
    assert!(shell(&v)["surfaces"]["prototype_player"].is_object(), "{}", shell(&v)["surfaces"]);
    #[cfg(feature = "desktop")]
    assert!(ak_node(&out, "Restart").is_some());
}

#[test]
fn source_monitor_surface_is_reachable_or_names_its_feature() {
    let scratch = Scratch::new("panels-source");
    let mut v = Viewer::new(Prefer::Cpu);
    let ctx = ctx();
    #[cfg(feature = "media")]
    {
        let media = scratch.0.join("clip.mov");
        std::fs::copy(format!("{}/../hsk-studio-reel/tests/fixtures/prores/prores-422hq-40x24-p.mov", env!("CARGO_MANIFEST_DIR")), &media).unwrap();
        let _ = open_surface(&mut v, &ctx, Panel::SourceMonitor);
        v.file_dialog().unwrap().typed = media.to_string_lossy().into_owned();
        assert_eq!(v.dialog_accept(), Ok(true), "{}", v.state_json()["status"]);
        let out = frame(&mut v, &ctx, vec![]);
        quiet(&ctx, &out, None);
        assert!(drew(&v, "source_monitor") && drew(&v, "source_monitor.mark_in"));
        assert_eq!(shell(&v)["surfaces"]["source_monitor"], v.source_monitor().unwrap().state_json());
        #[cfg(feature = "desktop")]
        assert!(ak_node(&out, "Mark In").is_some() && ak_node(&out, "Overwrite").is_some());
    }
    #[cfg(not(feature = "media"))]
    {
        let _ = (&scratch, &ctx);
        assert_eq!(shell(&v)["panels"]["source_monitor"]["availability"], "needs_feature");
        assert!(v.toggle_panel(Panel::SourceMonitor).unwrap_err().contains("needs feature media"));
    }
}

#[test]
fn audio_surfaces_are_reachable_or_name_their_feature() {
    let scratch = Scratch::new("panels-audio");
    let mut v = Viewer::new(Prefer::Cpu);
    let ctx = ctx();
    #[cfg(feature = "audio-batch")]
    {
        v.open(&sequence_package(&scratch.0)).unwrap();
        let wav = scratch.0.join("tone.wav");
        std::fs::write(&wav, support_audio::wav(2, 48_000, support_audio::WavFormat::Float32, None, &support_audio::signal(4800, 2))).unwrap();
        v.open(&wav).unwrap();
        for (p, id, state) in [(Panel::WaveformEditor, "waveform.panel", "waveform"), (Panel::SpectralDisplay, "spectral.panel", "spectral"), (Panel::Mixer, "mixer.panel", "mixer")] {
            let _out = open_surface(&mut v, &ctx, p);
            assert_eq!(shell(&v)["panels"][p.key()]["visible"], true, "{p:?}: {}", v.state_json()["status"]);
            assert!(drew(&v, id), "{id}: {:?}", v.drawn());
            assert!(!v.state_json()[state].is_null(), "{state} projected");
        }
        #[cfg(feature = "desktop")]
        {
            let out = frame(&mut v, &ctx, vec![]);
            assert!(ak_node(&out, "Heal").is_some() && ak_node(&out, "Master").is_some(), "spectral and mixer controls in the AccessKit tree");
        }
    }
    #[cfg(not(feature = "audio-batch"))]
    {
        let _ = (&scratch, &ctx);
        for p in [Panel::Mixer, Panel::WaveformEditor, Panel::SpectralDisplay] {
            assert_eq!(shell(&v)["panels"][p.key()]["availability"], "needs_feature");
            assert!(v.toggle_panel(p).unwrap_err().contains("needs feature audio-batch"));
        }
    }
}
