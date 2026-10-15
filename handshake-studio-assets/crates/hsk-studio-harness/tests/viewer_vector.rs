//! SMB-ILLUSTRATOR-PEN slice 4 (feature `viewer`): pen, direct selection and shape tools on
//! vector layers, live shape parameters, pathfinder, offset path and outline stroke. Every edit is
//! one chronicle transaction (undo/redo), drawn through composite's vector lowering (nib coverage
//! and strokes) by render-cpu, and survives save and reopen.
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_folio::{Payload, StudioShapeParameters};
use hsk_studio_harness::view::edit::Tool;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::{self, PathEdit, ShapeKind, VectorEntry, VectorTool};
use hsk_studio_harness::view::{Viewer, widget};
use hsk_studio_nib::anchor_path::{AnchorAddr, Constraint, HandleSide};
use hsk_studio_nib::pathfinder::PathfinderOp;
use hsk_studio_nib::stroke::LineJoin;
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

/// Red channel of pixel (x, y): 255 = paper, low = ink (black or the default red fill keeps r high,
/// so the tests paint black).
fn r(rgba: &[u8], x: usize, y: usize) -> u8 {
    rgba[(y * W + x) * 4]
}

fn black(fill: bool, stroke: bool) -> VectorTool {
    VectorTool { fill_on: fill, fill: [0, 0, 0], stroke_on: stroke, stroke: [0, 0, 0], stroke_width: 2.0, ..VectorTool::default() }
}

fn top_vector(v: &Viewer) -> String {
    v.layers().into_iter().find(|r| r.kind == "vector").expect("a vector layer").layer_id
}

fn revision(v: &Viewer) -> u64 {
    v.run().snapshot().unwrap().document().revision
}

#[test]
fn pen_clicks_and_drags_build_a_closed_path_one_transaction_each() {
    let scratch = Scratch::new("vector-pen");
    let mut v = paper(&scratch);
    v.set_vector_tool(black(false, true));
    v.set_tool(Tool::Pen);
    let r0 = revision(&v);
    v.pen_point(10.0, 10.0, None, 3.0).unwrap();
    v.pen_point(60.0, 10.0, None, 3.0).unwrap();
    v.pen_point(60.0, 50.0, Some([0.0, 10.0]), 3.0).unwrap();
    let id = v.pen_point(11.0, 11.0, None, 3.0).unwrap();
    assert_eq!(revision(&v), r0 + 4, "one transaction per click");
    assert_eq!(v.vector_tool().pen_layer, None, "closing ends the pen path");
    let layer = v.run().snapshot().unwrap().document().layers.iter().find(|l| l.layer_id == id).unwrap().clone();
    assert_eq!(layer.kind, "vector");
    assert!(matches!(&layer.payload, Payload::Vector { path_ids } if path_ids.len() == 1 && path_ids[0].starts_with("SVPT-")));
    let path = v.vector_path(&id).unwrap().path;
    assert_eq!(path.contours.len(), 1);
    assert!(path.contours[0].closed);
    let a = &path.contours[0].anchors;
    assert_eq!(a.iter().map(|a| a.position).collect::<Vec<_>>(), vec![PathPoint::new(10.0, 10.0), PathPoint::new(60.0, 10.0), PathPoint::new(60.0, 50.0)]);
    // The drag made a smooth anchor with mirrored handles.
    assert_eq!((a[2].mirroring, a[2].outgoing, a[2].incoming), (Mirroring::AngleAndLength, PathPoint::new(0.0, 10.0), PathPoint::new(-0.0, -10.0)));
    // Stroke only: the top edge is inked (2 px centred on y = 10), the interior is paper.
    let drawn = frame(&mut v);
    assert!(r(&drawn, 35, 9) < 60 && r(&drawn, 35, 10) < 60, "stroke on the top edge");
    assert_eq!(r(&drawn, 40, 20), 255, "no fill");
    assert_eq!(r(&drawn, 35, 13), 255, "nothing beyond the stroke");
    // Undo reopens the path (three anchors, open), redo closes it again with the same pixels.
    v.undo().unwrap();
    let open = v.vector_path(&top_vector(&v)).unwrap().path;
    assert!(!open.contours[0].closed);
    assert_eq!(open.contours[0].anchors.len(), 3);
    v.redo().unwrap();
    assert_eq!(frame(&mut v), drawn);
    // A new click after closing starts a second path layer.
    v.pen_point(80.0, 5.0, None, 3.0).unwrap();
    assert_eq!(v.layers().iter().filter(|r| r.kind == "vector").count(), 2);
}

#[test]
fn direct_selection_moves_anchors_bends_segments_and_drags_handles() {
    let scratch = Scratch::new("vector-direct");
    let mut v = paper(&scratch);
    v.set_vector_tool(black(true, false));
    let rect = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 40.0], false).unwrap();
    assert!(matches!(v.live_shape(&rect).unwrap(), Some(StudioShapeParameters::Rectangle { .. })));
    let drawn = frame(&mut v);
    assert!(r(&drawn, 30, 25) < 10 && r(&drawn, 60, 25) == 255 && r(&drawn, 30, 45) == 255, "filled 40 x 30 rectangle");
    // Drag the bottom-right anchor 20 px right: one transaction, the live shape is expanded.
    let r0 = revision(&v);
    let moved = v.direct_drag([50.0, 40.0], [70.0, 40.0], Constraint::Free, false, 3.0).unwrap();
    assert_eq!(revision(&v), r0 + 1);
    assert_eq!(moved, rect, "a path edit keeps the layer id (chronicle vector_paths)");
    assert_eq!(v.layers().iter().filter(|r| r.kind == "vector").count(), 1);
    assert_eq!(v.vector_path(&moved).unwrap().path.contours[0].anchors[2].position, PathPoint::new(70.0, 40.0));
    assert_eq!(v.live_shape(&moved).unwrap(), None, "direct edit expands the live shape");
    assert!(r(&frame(&mut v), 62, 38) < 10, "the moved corner widens the fill");
    // Drag the middle of the top edge up 10 px: the segment bends through the target.
    let bent = v.direct_drag([30.0, 10.0], [30.0, 0.0], Constraint::Free, false, 3.0).unwrap();
    let path = v.vector_path(&bent).unwrap().path;
    let mid = hsk_studio_nib::anchor_path::point_at(&path, 0, 0, 0.5).unwrap();
    assert!((mid.x - 30.0).abs() < 1e-6 && mid.y.abs() < 1e-6, "{mid:?}");
    assert!(r(&frame(&mut v), 30, 4) < 60, "the bulge is filled");
    // Convert the top-left anchor to smooth, then drag its out handle with Shift (45 degrees).
    let smooth = v.edit_path(&bent, PathEdit::Convert(AnchorAddr { contour: 0, anchor: 0 }, Mirroring::AngleAndLength)).unwrap();
    let a0 = v.vector_path(&smooth).unwrap().path.contours[0].anchors[0];
    let end = a0.handle_point(HandleSide::Out);
    let dragged = v.direct_drag([end.x, end.y], [end.x + 1.0, end.y + 5.0], Constraint::Angle45, false, 2.0).unwrap();
    let a0 = v.vector_path(&dragged).unwrap().path.contours[0].anchors[0];
    assert_eq!(a0.outgoing.x, 0.0, "snapped to the vertical: {a0:?}");
    assert_eq!(a0.incoming, PathPoint::new(-a0.outgoing.x, -a0.outgoing.y), "mirrored");
    // Undo walks back exactly: the bent path, then the moved anchor, then the live rectangle.
    v.undo().unwrap();
    v.undo().unwrap();
    assert_eq!(v.vector_path(&top_vector(&v)).unwrap().path, path);
    v.undo().unwrap();
    v.undo().unwrap();
    assert!(v.live_shape(&top_vector(&v)).unwrap().is_some());
    assert_eq!(frame(&mut v), drawn);
}

#[test]
fn shape_tools_draw_live_shapes_and_parameters_stay_editable() {
    let scratch = Scratch::new("vector-shapes");
    let mut v = paper(&scratch);
    v.set_vector_tool(black(true, false));
    let circle = v.draw_shape(ShapeKind::Ellipse, [10.0, 10.0], [50.0, 50.0], false).unwrap();
    let px = frame(&mut v);
    assert!(r(&px, 30, 30) < 10 && r(&px, 30, 12) < 10, "inside the circle");
    assert_eq!(r(&px, 12, 12), 255, "outside the circle in the bounding-box corner");
    let kinds: Vec<&str> = [ShapeKind::Polygon, ShapeKind::Star, ShapeKind::RoundedRectangle, ShapeKind::Line]
        .into_iter()
        .map(|k| {
            let id = v.draw_shape(k, [70.0, 20.0], [80.0, 30.0], false).unwrap();
            let live = v.live_shape(&id).unwrap().unwrap();
            match live {
                StudioShapeParameters::Polygon { side_count, .. } => {
                    assert_eq!(side_count, 6);
                    "polygon"
                }
                StudioShapeParameters::Star { point_count, .. } => {
                    assert_eq!(point_count, 5);
                    "star"
                }
                StudioShapeParameters::Rectangle { corner_radius, .. } => {
                    // Requested 8, clamped to half of the 10 px edge (STU-VEC-066) and reported.
                    assert_eq!(corner_radius.map(|l| l.value), [5.0; 4]);
                    "rectangle"
                }
                StudioShapeParameters::Line { length, angle, .. } => {
                    assert!((length.value - 200.0_f64.sqrt()).abs() < 1e-9 && (angle - 45.0).abs() < 1e-9);
                    "line"
                }
                StudioShapeParameters::Ellipse { .. } => "ellipse",
            }
        })
        .collect();
    assert_eq!(kinds, ["polygon", "star", "rectangle", "line"]);
    // The entry validates through Folio (live records against the document).
    let entry = v.vector_entry().unwrap();
    assert_eq!(entry.applicable_live(v.run().snapshot().unwrap().document()).shapes.len(), 5);
    // Live parameter edit: the circle becomes a ring (inner ratio 0.5), still live; one transaction.
    let StudioShapeParameters::Ellipse { center_x, center_y, radius_x, radius_y, .. } = v.live_shape(&circle).unwrap().unwrap() else { panic!() };
    let ring = v.set_live_shape(&circle, StudioShapeParameters::Ellipse { center_x, center_y, radius_x, radius_y, start_angle: 0.0, end_angle: 360.0, inner_ratio: 0.5 }).unwrap();
    assert!(matches!(v.live_shape(&ring).unwrap(), Some(StudioShapeParameters::Ellipse { inner_ratio, .. }) if inner_ratio == 0.5));
    let px = frame(&mut v);
    assert_eq!(r(&px, 30, 30), 255, "the hole is paper");
    assert!(r(&px, 30, 13) < 10, "the ring is inked");
    // Expand (explicit, history-tracked): plain geometry, no live record; undo makes it live again.
    let plain = v.expand_shape(&ring).unwrap();
    assert_eq!(v.live_shape(&plain).unwrap(), None);
    assert_eq!(v.vector_path(&plain).unwrap().path.contours.len(), 2);
    v.undo().unwrap();
    assert!(v.live_shape(&ring).unwrap().is_some());
}

#[test]
fn pathfinder_offset_and_outline_stroke_in_the_viewer() {
    let scratch = Scratch::new("vector-pathfinder");
    let mut v = paper(&scratch);
    v.set_vector_tool(black(true, false));
    let a = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [40.0, 40.0], false).unwrap();
    let b = v.draw_shape(ShapeKind::Rectangle, [25.0, 25.0], [55.0, 55.0], false).unwrap();
    let before = frame(&mut v);
    let r0 = revision(&v);
    let united = v.pathfinder_layers(PathfinderOp::Union, &[b.clone(), a.clone()]).unwrap();
    assert_eq!(revision(&v), r0 + 1, "one transaction");
    assert_eq!(united.len(), 1);
    assert_eq!(v.layers().iter().filter(|r| r.kind == "vector").count(), 1, "operands removed");
    let px = frame(&mut v);
    assert_eq!(px, before, "the union covers exactly the two rectangles");
    v.undo().unwrap();
    assert_eq!(v.layers().iter().filter(|r| r.kind == "vector").count(), 2);
    // Divide: three faces (overlap, and each remainder), each its own layer.
    let faces = v.pathfinder_layers(PathfinderOp::Divide, &[a.clone(), b.clone()]).unwrap();
    assert_eq!(faces.len(), 3);
    assert_eq!(frame(&mut v), before);
    v.undo().unwrap();
    // Offset path: a copy 4 px out (miter): bounds 6..44.
    let off = v.offset_path_layer(&a, 4.0, LineJoin::Miter).unwrap();
    let path = v.vector_path(&off).unwrap().path;
    let xs: Vec<f64> = path.contours.iter().flat_map(|c| c.anchors.iter().map(|a| a.position.x)).collect();
    assert_eq!((xs.iter().cloned().fold(f64::MAX, f64::min), xs.iter().cloned().fold(f64::MIN, f64::max)), (6.0, 44.0));
    assert_eq!(v.layers().iter().filter(|r| r.kind == "vector").count(), 3, "offset is a copy");
    // Outline stroke of a stroked line: a filled outline in the stroke colour, same pixels.
    v.set_vector_tool(VectorTool { stroke_width: 4.0, ..black(false, true) });
    let line = v.draw_shape(ShapeKind::Line, [60.0, 5.0], [90.0, 5.0], false).unwrap();
    let stroked = frame(&mut v);
    let outlined = v.outline_stroke_layer(&line).unwrap();
    let rec = v.vector_path(&outlined).unwrap();
    assert_eq!((rec.style.fill, rec.style.stroke), (Some([0, 0, 0]), None));
    assert_eq!(frame(&mut v), stroked, "outline stroke draws the same pixels");
}

#[test]
fn hostile_input_refusals_save_reopen_and_headless_ui() {
    let scratch = Scratch::new("vector-hostile");
    let mut v = paper(&scratch);
    v.set_vector_tool(black(true, false));
    let paper_layer = v.layers()[0].layer_id.clone();
    let rect = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [30.0, 30.0], false).unwrap();
    let r0 = revision(&v);
    assert!(v.pen_point(f64::NAN, 1.0, None, 3.0).is_err());
    assert!(v.draw_shape(ShapeKind::Rectangle, [0.0, 0.0], [f64::INFINITY, 5.0], false).is_err());
    assert!(v.draw_shape(ShapeKind::Rectangle, [0.0, 0.0], [20000.0, 5.0], false).unwrap_err().contains("shape_width"));
    v.set_vector_tool(VectorTool { polygon_sides: 2, ..black(true, false) });
    assert!(v.draw_shape(ShapeKind::Polygon, [40.0, 40.0], [50.0, 50.0], false).unwrap_err().contains("shape_side_count"));
    assert!(v.pathfinder_layers(PathfinderOp::Union, &[rect.clone(), paper_layer.clone()]).unwrap_err().contains("not_a_vector_layer"));
    assert!(v.pathfinder_layers(PathfinderOp::HardMix, &[rect.clone(), rect.clone()]).is_err());
    assert!(v.edit_path(&paper_layer, PathEdit::Reverse(None)).is_err());
    assert!(v.edit_path(&rect, PathEdit::DeleteAnchor(AnchorAddr { contour: 0, anchor: 99 })).unwrap_err().contains("point_index_out_of_bounds"));
    assert!(v.direct_drag([90.0, 60.0], [91.0, 60.0], Constraint::Free, false, 2.0).is_err(), "nothing under the pointer");
    assert_eq!(revision(&v), r0, "refusals change nothing");
    // Hostile entry bytes are refused typed, before per-path decoding.
    let doc = v.run().snapshot().unwrap().document().clone();
    let cancel = hsk_studio_accord::CancellationToken::default();
    let refuse = |bytes: String| VectorEntry::decode(bytes.as_bytes(), &doc, &cancel).unwrap_err();
    assert!(refuse(r#"{"schema":"other","paths":[],"live_shapes":null}"#.into()).contains("vector_entry_schema"));
    assert!(refuse(format!(r#"{{"schema":"{}","paths":[],"live_shapes":null,"x":1}}"#, vector::ENTRY_SCHEMA)).contains("unknown_field"));
    let many = vec!["{}"; vector::MAX_PATHS + 1].join(",");
    assert!(refuse(format!(r#"{{"schema":"{}","paths":[{many}],"live_shapes":null}}"#, vector::ENTRY_SCHEMA)).contains("over_budget"));
    let nan_path = r#"{"path_id":"SVPT-019abcde-0000-7000-8000-000000000001","fill":null,"stroke":null,"stroke_width":1.0,"path":{"fill_rule":"nonzero","contours":[{"closed":false,"anchors":[[1e999,0,0,0,0,0,"NONE"]]}]}}"#;
    assert!(VectorEntry::decode(format!(r#"{{"schema":"{}","paths":[{nan_path}],"live_shapes":null}}"#, vector::ENTRY_SCHEMA).as_bytes(), &doc, &cancel).is_err());
    // Save and reopen: records and pixels come back once the session carries the `vector` entry
    // token; until then saving a document with vector layers is refused (no silent geometry loss).
    let drawn = frame(&mut v);
    let out = scratch.0.join("vector.handshake");
    if vector::entry_persistable() {
        v.save(&out, false).unwrap();
        let mut again = Viewer::new(Prefer::Cpu);
        again.open(&out).unwrap();
        assert_eq!(again.vector_entry().unwrap(), v.vector_entry().unwrap());
        assert_eq!(frame(&mut again), drawn);
    } else {
        assert!(v.save(&out, false).unwrap_err().contains("vector_entry_not_persistable"));
        assert!(!out.exists(), "nothing written");
        println!("NOT_PROVEN: vector save/reopen needs session DOCUMENT_ENTRY_TOKENS to carry \"vector\" (lead request, lane SMB-ILLUSTRATOR-PEN)");
    }
    // Headless UI frame with the pen selected: vector tools and options are drawn with stable ids.
    v.set_tool(Tool::Pen);
    let ctx = egui::Context::default();
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0))), ..Default::default() };
    let _ = ctx.run(input, |ctx| v.ui(ctx));
    let drawn_ids = v.drawn().to_vec();
    for tool in [Tool::Pen, Tool::DirectSelect, Tool::Rectangle, Tool::RoundedRectangle, Tool::EllipseShape, Tool::Polygon, Tool::Star, Tool::Line] {
        assert!(drawn_ids.contains(&widget::tool(tool.key())), "{tool:?}: {drawn_ids:?}");
    }
    for name in [vector::widget::FILL, vector::widget::STROKE, vector::widget::STROKE_WIDTH, vector::widget::PEN_END] {
        assert!(drawn_ids.iter().any(|d| d == name), "{name}: {drawn_ids:?}");
    }
    assert_eq!(v.state_json()["edit"]["vector"]["pathfinder_ops"].as_array().map(Vec::len), Some(13));
    assert_eq!(v.state_json()["edit"]["tool"], "pen");
}
