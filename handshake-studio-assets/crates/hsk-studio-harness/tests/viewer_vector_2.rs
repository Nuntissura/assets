//! SMB-ILLUSTRATOR-PEN-2 slice 4 (feature `viewer`): curvature, pencil, knife and shape-builder
//! tools, live corners, arc segment / grids / spiral shapes; one chronicle transaction per edit,
//! rendered by render-cpu, saved and reopened.
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_folio::StudioShapeParameters;
use hsk_studio_harness::view::edit::Tool;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::{PathEdit, ShapeKind, VectorTool};
use hsk_studio_harness::view::{Viewer, widget};
use hsk_studio_nib::anchor_path::AnchorAddr;
use hsk_studio_nib::freeform::CornerTreatment;
use hsk_studio_nib::{Mirroring, PathPoint};

const W: usize = 96;
const H: usize = 64;

fn paper(scratch: &Scratch) -> Viewer {
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(W as u32, H as u32, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v
}

fn frame(v: &mut Viewer) -> Vec<u8> {
    v.render_if_dirty();
    v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).rgba8.clone()
}

fn r(rgba: &[u8], x: usize, y: usize) -> u8 {
    rgba[(y * W + x) * 4]
}

fn black(fill: bool, stroke: bool) -> VectorTool {
    VectorTool { fill_on: fill, fill: [0, 0, 0], stroke_on: stroke, stroke: [0, 0, 0], stroke_width: 2.0, ..VectorTool::default() }
}

fn revision(v: &Viewer) -> u64 {
    v.run().snapshot().unwrap().document().revision
}

fn vectors(v: &Viewer) -> usize {
    v.layers().iter().filter(|r| r.kind == "vector").count()
}

#[test]
fn curvature_points_and_pencil_strokes() {
    let scratch = Scratch::new("vector2-curve");
    let mut v = paper(&scratch);
    v.set_vector_tool(black(false, true));
    v.set_tool(Tool::Curvature);
    let r0 = revision(&v);
    let id = v.curvature_point(10.0, 40.0, false, 3.0).unwrap();
    assert_eq!(v.curvature_point(30.0, 10.0, false, 3.0).unwrap(), id, "the same layer grows");
    assert_eq!(v.curvature_point(50.0, 40.0, false, 3.0).unwrap(), id);
    assert_eq!(v.curvature_point(70.0, 10.0, true, 3.0).unwrap(), id);
    assert_eq!(revision(&v), r0 + 4);
    let a = v.vector_path(&id).unwrap().path.contours[0].anchors.clone();
    // The curve passes through the clicked points; interior smooth points carry (next - prev) / 6.
    assert_eq!(a.iter().map(|a| a.position).collect::<Vec<_>>(), vec![PathPoint::new(10.0, 40.0), PathPoint::new(30.0, 10.0), PathPoint::new(50.0, 40.0), PathPoint::new(70.0, 10.0)]);
    assert_eq!(a[1].mirroring, Mirroring::AngleAndLength);
    assert!((a[1].outgoing.x - 40.0 / 6.0).abs() < 1e-12 && a[1].outgoing.y == 0.0, "{:?}", a[1].outgoing);
    assert_eq!((a[3].mirroring, a[3].incoming), (Mirroring::None, PathPoint::new(0.0, 0.0)), "Alt-click = corner");
    // Undo removes the last point only.
    v.undo().unwrap();
    assert_eq!(v.vector_path(&id).unwrap().path.contours[0].anchors.len(), 3);
    // Pencil: a jittered horizontal drag becomes a 2-anchor path; its stroke is drawn.
    v.pen_end();
    v.set_tool(Tool::Pencil);
    let samples: Vec<[f64; 2]> = (0..=40).map(|i| [20.0 + f64::from(i), if i % 2 == 0 { 55.2 } else { 54.8 }]).collect();
    let pencil = v.pencil_stroke(&samples).unwrap();
    assert_eq!(v.vector_path(&pencil).unwrap().path.contours[0].anchors.len(), 2);
    let px = frame(&mut v);
    assert!(r(&px, 40, 55) < 60, "pencil stroke inked (2 px centred on y = 55.2)");
    assert!(v.pencil_stroke(&[[f64::NAN, 1.0], [2.0, 2.0]]).unwrap_err().contains("non_finite"));
}

#[test]
fn knife_cuts_and_live_corners() {
    let scratch = Scratch::new("vector2-knife");
    let mut v = paper(&scratch);
    v.set_vector_tool(black(true, false));
    let rect = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 40.0], false).unwrap();
    let before = frame(&mut v);
    // Round the top-left corner (radius 8): the corner pixel becomes paper; one transaction, same id.
    let r0 = revision(&v);
    let rounded = v.edit_path(&rect, PathEdit::Corner { at: AnchorAddr { contour: 0, anchor: 0 }, kind: CornerTreatment::Round, radius: 8.0 }).unwrap();
    assert_eq!((rounded.as_str(), revision(&v)), (rect.as_str(), r0 + 1));
    assert_eq!(v.vector_path(&rect).unwrap().path.contours[0].anchors.len(), 5);
    let px = frame(&mut v);
    assert_eq!(r(&px, 10, 10), 255, "rounded off");
    assert!(r(&px, 30, 25) < 10);
    v.undo().unwrap();
    assert_eq!(frame(&mut v), before);
    // Knife along x = 30: two closed regions replace the layer in one transaction.
    let r0 = revision(&v);
    let pieces = v.knife_cut(&[[30.0, 0.0], [30.0, 60.0]]).unwrap();
    assert_eq!((pieces.len(), vectors(&v), revision(&v)), (2, 2, r0 + 1));
    let px = frame(&mut v);
    assert!(r(&px, 20, 25) < 10 && r(&px, 40, 25) < 10, "both halves stay filled");
    v.undo().unwrap();
    assert_eq!(vectors(&v), 1);
    // A cut that misses everything is refused typed and changes nothing.
    let r0 = revision(&v);
    assert!(v.knife_cut(&[[80.0, 0.0], [80.0, 60.0]]).unwrap_err().contains("nothing cut"));
    assert_eq!(revision(&v), r0);
}

#[test]
fn shape_builder_merges_and_deletes_regions() {
    let scratch = Scratch::new("vector2-builder");
    let mut v = paper(&scratch);
    v.set_vector_tool(black(true, false));
    let a = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [40.0, 40.0], false).unwrap();
    let b = v.draw_shape(ShapeKind::Rectangle, [25.0, 25.0], [55.0, 55.0], false).unwrap();
    let before = frame(&mut v);
    let operands = v.stack_vector_layers();
    assert_eq!(operands, vec![a.clone(), b.clone()]);
    // Drag across everything: one merged layer, same pixels.
    let merged = v.shape_builder_layers(&operands, &[[12.0, 12.0], [52.0, 52.0]], false).unwrap();
    assert_eq!((merged.len(), vectors(&v)), (1, 1));
    assert_eq!(frame(&mut v), before);
    v.undo().unwrap();
    // Alt-click the overlap: it is deleted, the two L shapes stay.
    let left = v.shape_builder_layers(&[a, b], &[[32.0, 32.0]], true).unwrap();
    assert_eq!(left.len(), 2);
    let px = frame(&mut v);
    assert_eq!(r(&px, 32, 32), 255, "overlap removed");
    assert!(r(&px, 15, 15) < 10 && r(&px, 50, 50) < 10);
}

#[test]
fn arc_grid_polar_grid_and_spiral_shapes_stay_live() {
    let scratch = Scratch::new("vector2-shapes");
    let mut v = paper(&scratch);
    v.set_vector_tool(VectorTool { grid_rows: 2, grid_columns: 3, ..black(true, true) });
    let grid = v.draw_shape(ShapeKind::RectangularGrid, [10.0, 10.0], [40.0, 40.0], false).unwrap();
    assert_eq!(v.vector_path(&grid).unwrap().path.contours.len(), 6, "frame + 2 rows + 3 columns");
    let kinds: Vec<&str> = [ShapeKind::ArcSegment, ShapeKind::PolarGrid, ShapeKind::Spiral]
        .into_iter()
        .map(|k| {
            let id = v.draw_shape(k, [60.0, 20.0], [80.0, 40.0], false).unwrap();
            v.live_shape(&id).unwrap().unwrap().kind()
        })
        .collect();
    assert_eq!(kinds, ["arc_segment", "polar_grid", "spiral"]);
    // Grid lines are drawn (stroke), cells are paper (grids draw no fill).
    let px = frame(&mut v);
    assert!(r(&px, 25, 20) < 60, "row divider at y = 20");
    assert_eq!(r(&px, 15, 15), 255);
    // Live parameter edit: 4 rows instead of 2, same layer, still live.
    let Some(StudioShapeParameters::RectangularGrid { x, y, width, height, vertical_dividers, skew_horizontal, skew_vertical, use_outside_rectangle_as_frame, fill_grid, .. }) = v.live_shape(&grid).unwrap() else { panic!() };
    let params = StudioShapeParameters::RectangularGrid { x, y, width, height, horizontal_dividers: 4, vertical_dividers, skew_horizontal, skew_vertical, use_outside_rectangle_as_frame, fill_grid };
    assert_eq!(v.set_live_shape(&grid, params).unwrap(), grid);
    assert_eq!(v.vector_path(&grid).unwrap().path.contours.len(), 8);
    // Folio refuses an out-of-domain parameter; nothing changes.
    let r0 = revision(&v);
    let spiral = v.layers().into_iter().find(|r| r.name == "Spiral").unwrap().layer_id;
    let Some(StudioShapeParameters::Spiral { center_x, center_y, radius, turns, clockwise, .. }) = v.live_shape(&spiral).unwrap() else { panic!() };
    assert!(v.set_live_shape(&spiral, StudioShapeParameters::Spiral { center_x, center_y, radius, decay: 0.0, turns, clockwise }).is_err());
    assert_eq!(revision(&v), r0);
    // Save and reopen keeps every live record and the pixels.
    let drawn = frame(&mut v);
    let out = scratch.0.join("shapes.handshake");
    v.save(&out, false).unwrap();
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&out).unwrap();
    assert_eq!(again.vector_entry().unwrap(), v.vector_entry().unwrap());
    assert_eq!(frame(&mut again), drawn);
}

#[test]
fn new_tools_are_drawn_with_stable_ids_and_refuse_typed() {
    let scratch = Scratch::new("vector2-ui");
    let mut v = paper(&scratch);
    for (tool, option) in [
        (Tool::Pencil, hsk_studio_harness::view::vector::widget::PENCIL_FIDELITY),
        (Tool::ArcSegment, hsk_studio_harness::view::vector::widget::ARC_SLOPE),
        (Tool::RectangularGrid, hsk_studio_harness::view::vector::widget::GRID_ROWS),
        (Tool::PolarGrid, hsk_studio_harness::view::vector::widget::POLAR_RADIAL),
        (Tool::Spiral, hsk_studio_harness::view::vector::widget::SPIRAL_TURNS),
        (Tool::Curvature, hsk_studio_harness::view::vector::widget::PEN_END),
    ] {
        v.set_tool(tool);
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1800.0, 900.0))), ..Default::default() };
        let _ = ctx.run(input, |ctx| v.ui(ctx));
        let drawn = v.drawn().to_vec();
        for t in [Tool::Curvature, Tool::Pencil, Tool::Knife, Tool::ShapeBuilder, Tool::ArcSegment, Tool::RectangularGrid, Tool::PolarGrid, Tool::Spiral] {
            assert!(drawn.contains(&widget::tool(t.key())), "{t:?}: {drawn:?}");
        }
        assert!(drawn.iter().any(|d| d == option), "{option}: {drawn:?}");
    }
    // Typed refusals change nothing.
    v.set_vector_tool(black(true, false));
    let rect = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [30.0, 30.0], false).unwrap();
    let r0 = revision(&v);
    assert!(v.edit_path(&rect, PathEdit::Corner { at: AnchorAddr { contour: 0, anchor: 0 }, kind: CornerTreatment::Chamfer, radius: f64::NAN }).unwrap_err().contains("non_finite"));
    assert!(v.curvature_point(f64::INFINITY, 1.0, false, 3.0).is_err());
    assert!(v.knife_cut(&[[1.0, 1.0]]).is_err());
    assert!(v.shape_builder_layers(std::slice::from_ref(&rect), &[[90.0, 60.0]], false).unwrap_err().contains("InvalidRequest"));
    v.set_vector_tool(VectorTool { pencil_fidelity: 9, ..black(true, false) });
    assert!(v.pencil_stroke(&[[0.0, 0.0], [5.0, 5.0]]).is_err());
    v.set_vector_tool(VectorTool { spiral_decay: 100.0, ..black(true, false) });
    assert!(v.draw_shape(ShapeKind::Spiral, [40.0, 40.0], [50.0, 50.0], false).unwrap_err().contains("shape_decay"));
    assert_eq!(revision(&v), r0);
}
