//! Lane SMB-AI-ARROWHEADS-2 slices 3 and 4 (feature `viewer`): stroke arrowheads authored in the
//! viewer (Folio `stroke_markers`, STU-VEC-146): set and swap are one undo step each, validated by
//! Folio, projected in `state_json`, drawn on the canvas by composite's Folio adapter, kept on their
//! stroke row through restyles, driven by the Stroke panel's `panel.stroke.marker.*` controls, and
//! written by the vector PDF export as nib's outlines. Expected pixels are hand-derived from the
//! geometry (document px, 1 px = 1 pixel) and nib's marker table (a Studio reading, UNVERIFIED D5).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_folio::{StudioArrowAlignment as Align, StudioArrowhead as A, StudioAppearanceRow};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::hosted::{self, HostedPanel, PanelAction};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::shell::{Panel, ShellCommand};
use hsk_studio_harness::view::vector::{MarkerSpec, PaintSpec, ShapeKind, StrokePanel, VectorTool};

const W: usize = 96;
const H: usize = 64;
const WHITE: [u8; 3] = [255, 255, 255];
const BLUE: [u8; 3] = [0, 0, 255];

fn paper(scratch: &Scratch) -> Viewer {
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(W as u32, H as u32, "paper", WHITE)).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: false, stroke_on: false, ..VectorTool::default() });
    v
}

/// A horizontal line from (10, 32) to (70, 32) with a 4 px blue stroke as its only row (row 0).
fn line(v: &mut Viewer) -> String {
    let id = v.draw_shape(ShapeKind::Line, [10.0, 32.0], [70.0, 32.0], false).unwrap();
    v.set_fill(&id, None).unwrap();
    v.set_stroke(&id, Some(&StrokePanel::solid(BLUE, 4.0))).unwrap();
    id
}

fn frame(v: &mut Viewer) -> Vec<u8> {
    v.render_if_dirty();
    v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).rgba8.clone()
}

fn px(f: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * W + x) * 4;
    [f[i], f[i + 1], f[i + 2]]
}

fn spec() -> MarkerSpec {
    MarkerSpec { start: A::ArrowTriangle, end: A::ArrowCircleSolid, start_scale: 200.0, end_scale: 100.0, alignment: Align::InsidePath }
}

fn projected(v: &Viewer) -> serde_json::Value {
    v.state_json()["vector"]["selected_path"]["stroke_markers"].clone()
}

#[test]
fn set_markers_is_one_undo_step_each_validated_and_projected() {
    let scratch = Scratch::new("markers-set");
    let mut v = paper(&scratch);
    let id = line(&mut v);
    v.select(Some(id.clone()));
    assert_eq!(v.stroke_markers(&id, 0).unwrap(), None);
    // Start arrow.triangle at 200 %, end arrow.circle_solid: one step.
    v.set_stroke_markers(&id, 0, Some(spec())).unwrap();
    assert_eq!(v.stroke_markers(&id, 0).unwrap(), Some(spec()));
    let p = projected(&v);
    assert_eq!((p[0]["row"].as_u64(), p[0]["start"].as_str(), p[0]["end"].as_str(), p[0]["start_scale"].as_f64()), (Some(0), Some("arrow.triangle"), Some("arrow.circle_solid"), Some(200.0)), "{p}");
    assert_eq!(p[0]["alignment"], "arrow_align.inside_path");
    // Alignment outside the path: one more step.
    let outside = MarkerSpec { alignment: Align::OutsidePath, ..spec() };
    v.set_stroke_markers(&id, 0, Some(outside)).unwrap();
    assert_eq!(v.stroke_markers(&id, 0).unwrap(), Some(outside));
    v.undo().unwrap();
    assert_eq!(v.stroke_markers(&id, 0).unwrap(), Some(spec()));
    v.undo().unwrap();
    assert_eq!(v.stroke_markers(&id, 0).unwrap(), None);
    v.redo().unwrap();
    assert_eq!(v.stroke_markers(&id, 0).unwrap(), Some(spec()));
    // Hostile or invalid edits are refused typed and change nothing (Folio's own checks).
    for bad in [
        MarkerSpec { start_scale: f64::NAN, ..spec() },
        MarkerSpec { end_scale: 0.5, ..spec() },
        MarkerSpec { start_scale: 1000.5, ..spec() },
        MarkerSpec { end: A::ErdOne, ..spec() },
    ] {
        assert!(v.set_stroke_markers(&id, 0, Some(bad)).is_err(), "{bad:?}");
    }
    assert!(v.set_stroke_markers(&id, 1, Some(spec())).is_err(), "no row 1");
    v.set_fill(&id, Some(&PaintSpec::Solid(BLUE))).unwrap();
    assert!(v.set_stroke_markers(&id, 0, Some(spec())).unwrap_err().contains("stroke"), "row 0 is now the fill row");
    assert_eq!(v.stroke_markers(&id, 1).unwrap(), Some(spec()));
    // Removing is one step too.
    v.set_stroke_markers(&id, 1, None).unwrap();
    assert_eq!(v.stroke_markers(&id, 1).unwrap(), None);
    v.undo().unwrap();
    assert_eq!(v.stroke_markers(&id, 1).unwrap(), Some(spec()));
}

#[test]
fn the_canvas_draws_the_markers_near_the_ends_only() {
    let scratch = Scratch::new("markers-render");
    let mut v = paper(&scratch);
    let id = line(&mut v);
    let plain = frame(&mut v);
    // End arrow.triangle at 100 %, inside the path: marker size 4, tip at (70, 32), base at x 58,
    // half width 6 (nib table). Pixel (60, 36) spans 10..11 px behind the tip (half width 5..5.5)
    // and 4..5 px off the line (the stroke's half width is 2): marker only.
    v.set_stroke_markers(&id, 0, Some(MarkerSpec::new(A::ArrowNone, A::ArrowTriangle, Align::InsidePath))).unwrap();
    let marked = frame(&mut v);
    assert_eq!(px(&plain, 60, 36), WHITE);
    assert_eq!(px(&marked, 60, 36), BLUE);
    // Every pixel that changed lies in the marker's box near the end; the start is untouched.
    let mut changed = 0;
    for y in 0..H {
        for x in 0..W {
            if px(&plain, x, y) != px(&marked, x, y) {
                changed += 1;
                assert!((57..=71).contains(&x) && (25..=39).contains(&y), "pixel ({x}, {y}) changed");
            }
        }
    }
    assert!(changed > 20, "{changed}");
}

#[test]
fn swap_exchanges_the_ends_in_one_step() {
    let scratch = Scratch::new("markers-swap");
    let mut v = paper(&scratch);
    let id = line(&mut v);
    v.set_stroke_markers(&id, 0, Some(spec())).unwrap();
    v.swap_stroke_markers(&id, 0).unwrap();
    let swapped = MarkerSpec { start: A::ArrowCircleSolid, end: A::ArrowTriangle, start_scale: 100.0, end_scale: 200.0, alignment: Align::InsidePath };
    assert_eq!(v.stroke_markers(&id, 0).unwrap(), Some(swapped));
    v.undo().unwrap();
    assert_eq!(v.stroke_markers(&id, 0).unwrap(), Some(spec()));
    assert!(v.swap_stroke_markers(&id, 3).is_err());
}

#[test]
fn restyles_keep_the_markers_on_their_stroke_row() {
    let scratch = Scratch::new("markers-restyle");
    let mut v = paper(&scratch);
    let id = line(&mut v);
    v.set_stroke_markers(&id, 0, Some(spec())).unwrap();
    // A new stroke panel (weight 6) keeps the markers on the stroke row.
    v.set_stroke(&id, Some(&StrokePanel::solid(BLUE, 6.0))).unwrap();
    assert_eq!(v.stroke_markers(&id, 0).unwrap(), Some(spec()));
    // A fill row goes in front: the stroke row is now row 1 and its markers follow it.
    v.set_fill(&id, Some(&PaintSpec::Solid(BLUE))).unwrap();
    assert!(matches!(v.layer_paint_style(&id).unwrap().rows[1], StudioAppearanceRow::Stroke { .. }));
    assert_eq!(v.stroke_markers(&id, 1).unwrap(), Some(spec()));
    // A graphic style made from the layer carries them; applying it to a second line and breaking
    // the link gives that line the same markers.
    let style = v.add_graphic_style("Arrow", &id).unwrap();
    let other = line(&mut v);
    v.apply_graphic_style(&other, &style).unwrap();
    v.break_graphic_style(&other).unwrap();
    assert_eq!(v.stroke_markers(&other, 1).unwrap(), Some(spec()));
    // Merging a style appends its rows (fills after the fills, strokes after the strokes: fill,
    // fill, stroke, stroke): the layer's own stroke row, now row 2, keeps its markers.
    v.merge_graphic_style(&id, &style).unwrap();
    assert_eq!(v.stroke_markers(&id, 2).unwrap(), Some(spec()));
    assert_eq!(v.stroke_markers(&id, 3).unwrap(), None);
    // Removing the stroke (or clearing the appearance) removes them with the row.
    v.clear_appearance(&other).unwrap();
    assert!(v.stroke_markers(&other, 0).is_err(), "no stroke row");
    let entry = v.vector_entry().unwrap();
    assert!(entry.records.stroke_markers.iter().all(|m| !m.style_id.ends_with(other.trim_start_matches("SLYR-"))), "{:?}", entry.records.stroke_markers);
}

#[test]
fn stroke_panel_marker_controls_drive_the_same_edits() {
    let scratch = Scratch::new("markers-panel");
    let mut v = paper(&scratch);
    let id = line(&mut v);
    v.select(Some(id.clone()));
    v.shell_command(ShellCommand::Panel(Panel::Hosted(HostedPanel::Stroke))).unwrap();
    let ctx = egui::Context::default();
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))), ..Default::default() };
    let _ = ctx.run(input, |ctx| v.ui(ctx));
    for control in ["marker.start", "marker.end", "marker.start_scale", "marker.end_scale", "marker.align", "marker.apply", "marker.none", "marker.swap"] {
        let name = hosted::widget::control("stroke", control);
        assert!(v.drawn().iter().any(|d| *d == name), "{name} not drawn: {:?}", v.drawn());
    }
    // The panel actions are the Viewer calls: the same record as a direct call.
    v.hosted_action(PanelAction::SetStrokeMarkers(Some(spec()))).unwrap();
    assert_eq!(v.stroke_markers(&id, 0).unwrap(), Some(spec()));
    v.hosted_action(PanelAction::SwapStrokeMarkers).unwrap();
    assert_eq!(v.stroke_markers(&id, 0).unwrap().map(|m| (m.start, m.end)), Some((A::ArrowCircleSolid, A::ArrowTriangle)));
    let state = v.state_json()["shell"]["hosted"]["stroke"].clone();
    assert_eq!(state["markers"]["start"], "arrow.circle_solid", "{state}");
    v.hosted_action(PanelAction::SetStrokeMarkers(None)).unwrap();
    assert_eq!(v.stroke_markers(&id, 0).unwrap(), None);
    v.select(None);
    assert!(v.hosted_action(PanelAction::SwapStrokeMarkers).unwrap_err().contains("select a layer"));
}

#[test]
fn vector_pdf_export_writes_the_marker_outlines() {
    let scratch = Scratch::new("markers-pdf");
    let mut v = paper(&scratch);
    let id = line(&mut v);
    let (plain, _) = v.vector_pdf_bytes().unwrap();
    v.set_stroke_markers(&id, 0, Some(MarkerSpec::new(A::ArrowNone, A::ArrowTriangle, Align::InsidePath))).unwrap();
    let (bytes, losses) = v.vector_pdf_bytes().unwrap();
    assert!(!losses.iter().any(|l| l.contains("marker")), "{losses:?}");
    // Rendered back at 4/3 px per pt = 1 px per document px: the marker-only pixel of the canvas
    // test is painted in the export and paper in the unmarked export.
    let cancel = hsk_studio_accord::CancellationToken::default();
    let render = |b: &[u8]| hsk_studio_press::render_page(b, 0, 4.0 / 3.0, &hsk_studio_press::Limits::default(), &cancel).unwrap();
    let (marked, plain) = (render(&bytes), render(&plain));
    assert_eq!(plain.pixel(60, 36), Some(WHITE));
    let c = marked.pixel(60, 36).unwrap();
    assert!(c[0] < 40 && c[1] < 40 && c[2] > 215, "{c:?}");
}
