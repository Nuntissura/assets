//! SMB-ILLUSTRATOR-PEN-3 (feature `viewer`): vector networks stored as Folio records and edited in
//! the viewer (promote, vertices and edges, paint bucket over crossings and T-junctions), the knife
//! on strokes, variable-width strokes, reshape and the blob brush; one chronicle transaction per
//! edit, rendered by render-cpu, saved and reopened.
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_harness::view::edit::Tool;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::{self, NetworkEdit, ShapeKind, VectorEntry, VectorTool};
use hsk_studio_harness::view::{Viewer, widget};
use hsk_studio_nib::anchor_path::Constraint;

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

fn px(rgba: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * W + x) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

/// Red fill (paint bucket colour), black 2 px strokes.
fn tool() -> VectorTool {
    VectorTool { fill_on: false, fill: [255, 0, 0], stroke_on: true, stroke: [0, 0, 0], stroke_width: 2.0, ..VectorTool::default() }
}

fn revision(v: &Viewer) -> u64 {
    v.run().snapshot().unwrap().document().revision
}

fn vectors(v: &Viewer) -> usize {
    v.layers().iter().filter(|r| r.kind == "vector").count()
}

const RED: [u8; 3] = [255, 0, 0];
const WHITE: [u8; 3] = [255, 255, 255];

#[test]
fn network_promote_edit_paint_bucket_undo_and_save() {
    let scratch = Scratch::new("vector3-network");
    let mut v = paper(&scratch);
    v.set_vector_tool(tool());
    let rect = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 40.0], false).unwrap();
    let r0 = revision(&v);
    assert_eq!(v.promote_network(&rect).unwrap(), rect, "same layer");
    let net = v.vector_network(&rect).unwrap();
    assert_eq!((net.vertices.len(), net.segments.len()), (4, 4));
    // A divider whose ends lie on the top and bottom edges (T-junctions, no shared vertices).
    v.edit_network(&rect, NetworkEdit::AddVertex { x: 30.0, y: 10.0 }).unwrap();
    v.edit_network(&rect, NetworkEdit::AddVertex { x: 30.0, y: 40.0 }).unwrap();
    v.edit_network(&rect, NetworkEdit::Connect { a: 4, b: 5 }).unwrap();
    assert_eq!(revision(&v), r0 + 4, "one transaction per edit");
    let before = frame(&mut v);
    assert_eq!(px(&before, 30, 25)[0], 0, "divider stroked");
    // Paint bucket the left face: only it is filled (red), the right face stays paper.
    v.network_paint_bucket(&rect, [20.0, 25.0], true).unwrap();
    assert_eq!(revision(&v), r0 + 5);
    let painted = frame(&mut v);
    assert_eq!((px(&painted, 20, 25), px(&painted, 40, 25)), (RED, WHITE));
    let net = v.vector_network(&rect).unwrap();
    assert_eq!(net.regions.iter().filter(|r| r.fills).count(), 1);
    assert_eq!(net.regions.len(), 2, "both faces carry their fill state");
    // The right face too, then clear the left one (Alt).
    v.network_paint_bucket(&rect, [40.0, 25.0], true).unwrap();
    v.network_paint_bucket(&rect, [20.0, 25.0], false).unwrap();
    let swapped = frame(&mut v);
    assert_eq!((px(&swapped, 20, 25), px(&swapped, 40, 25)), (WHITE, RED));
    // Undo walks back one step at a time.
    v.undo().unwrap();
    v.undo().unwrap();
    assert_eq!(frame(&mut v), painted);
    // Direct selection drags a network vertex (the divider's bottom end): one transaction.
    v.select(Some(rect.clone()));
    let r1 = revision(&v);
    v.direct_drag([30.0, 40.0], [34.0, 40.0], Constraint::Free, false, 2.0).unwrap();
    assert_eq!(revision(&v), r1 + 1);
    assert!(v.vector_network(&rect).unwrap().vertices.iter().any(|q| q.x == 34.0 && q.y == 40.0));
    // Save and reopen: the Folio network record and the pixels come back.
    let drawn = frame(&mut v);
    let out = scratch.0.join("network.handshake");
    v.save(&out, false).unwrap();
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&out).unwrap();
    assert_eq!(again.vector_entry().unwrap(), v.vector_entry().unwrap());
    assert!(again.vector_entry().unwrap().paths.iter().any(|p| p.network.is_some()));
    assert_eq!(frame(&mut again), drawn);
}

#[test]
fn paint_bucket_finds_faces_across_crossings() {
    let scratch = Scratch::new("vector3-cross");
    let mut v = paper(&scratch);
    v.set_vector_tool(tool());
    let rect = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 50.0], false).unwrap();
    v.promote_network(&rect).unwrap();
    // Both diagonals, crossing at (30, 30) with no vertex there.
    v.edit_network(&rect, NetworkEdit::Connect { a: 0, b: 2 }).unwrap();
    v.edit_network(&rect, NetworkEdit::Connect { a: 1, b: 3 }).unwrap();
    // The top triangle (between the diagonals, above the crossing).
    v.network_paint_bucket(&rect, [30.0, 15.0], true).unwrap();
    let p = frame(&mut v);
    assert_eq!(px(&p, 30, 16), RED);
    assert_eq!((px(&p, 30, 44), px(&p, 14, 30), px(&p, 46, 30)), (WHITE, WHITE, WHITE));
    assert_eq!(v.vector_network(&rect).unwrap().regions.len(), 4, "the crossing split the square into four faces");
    // Outside every face: typed refusal, nothing changes.
    let r0 = revision(&v);
    assert!(v.network_paint_bucket(&rect, [80.0, 30.0], true).unwrap_err().contains("no enclosed region"));
    assert_eq!(revision(&v), r0);
}

#[test]
fn knife_on_strokes_width_profile_and_fill_stroke_pattern() {
    let scratch = Scratch::new("vector3-width");
    let mut v = paper(&scratch);
    v.set_vector_tool(tool());
    // Knife across a stroke-only line: two open pieces, one transaction.
    let line = v.draw_shape(ShapeKind::Line, [10.0, 20.0], [80.0, 20.0], false).unwrap();
    let r0 = revision(&v);
    let pieces = v.knife_cut(&[[40.0, 10.0], [40.0, 30.0]]).unwrap();
    assert_eq!((pieces.len(), vectors(&v), revision(&v)), (2, 2, r0 + 1));
    assert!(pieces.iter().all(|id| !v.vector_path(id).unwrap().path.contours[0].closed));
    v.undo().unwrap();
    assert_eq!(vectors(&v), 1);
    // Width tool: 12 px in the middle of the 2 px line (same layer, one transaction).
    let thin = frame(&mut v);
    assert_eq!(px(&thin, 45, 24), WHITE);
    let r1 = revision(&v);
    assert_eq!(v.set_width_point(&line, 0.5, 12.0).unwrap(), line);
    assert_eq!(revision(&v), r1 + 1);
    let wide = frame(&mut v);
    assert_eq!(px(&wide, 45, 24), [0, 0, 0], "6 px below the centre line at mid length");
    assert_eq!(px(&wide, 12, 24), WHITE, "still thin at the start");
    // A width drag through the tool gesture (2 * distance) on the same point.
    v.width_drag([45.0, 20.0], [45.0, 22.0], 3.0).unwrap();
    let profile = v.vector_path(&line).unwrap().width.unwrap();
    assert!(profile.iter().any(|p| (p[0] - 0.5).abs() < 0.02 && (p[1] - 4.0).abs() < 1e-9), "{profile:?}");
    v.clear_width(&line).unwrap();
    assert_eq!(frame(&mut v), thin);
    // Fill and variable stroke together: red interior, black outline, through one colour tile.
    v.set_vector_tool(VectorTool { fill_on: true, stroke_width: 4.0, ..tool() });
    let rect = v.draw_shape(ShapeKind::Rectangle, [20.0, 35.0], [60.0, 60.0], false).unwrap();
    v.set_width_point(&rect, 0.0, 4.0).unwrap();
    let both = frame(&mut v);
    assert_eq!(px(&both, 40, 47), RED);
    assert_eq!(px(&both, 40, 35), [0, 0, 0], "stroke paints above the fill");
}

#[test]
fn reshape_blob_brush_and_new_tool_ids() {
    let scratch = Scratch::new("vector3-blob");
    let mut v = paper(&scratch);
    v.set_vector_tool(VectorTool { blob_diameter: 6.0, fill: [0, 0, 0], ..tool() });
    // Reshape the middle of a line up by 10: same layer, the grabbed point follows.
    let line = v.draw_shape(ShapeKind::Line, [10.0, 40.0], [80.0, 40.0], false).unwrap();
    assert_eq!(v.reshape_drag([45.0, 40.0], [45.0, 30.0], 3.0).unwrap(), line);
    let path = v.vector_path(&line).unwrap().path;
    let hit = hsk_studio_nib::anchor_path::hit_segment(&path, hsk_studio_nib::PathPoint::new(45.0, 30.0), 0.5);
    assert!(hit.is_some(), "the path passes through the target");
    // Blob brush: a stroke, an overlapping stroke merges into it, a far one stays separate.
    let first = v.blob_brush_stroke(&[[10.0, 10.0], [30.0, 10.0]]).unwrap();
    let blobs = |v: &Viewer| v.layers().iter().filter(|r| r.name == "Blob").count();
    assert_eq!(blobs(&v), 1);
    let merged = v.blob_brush_stroke(&[[28.0, 10.0], [28.0, 25.0]]).unwrap();
    assert_eq!(blobs(&v), 1, "merged into one shape");
    assert_ne!(merged, first);
    v.blob_brush_stroke(&[[70.0, 10.0], [85.0, 10.0]]).unwrap();
    assert_eq!(blobs(&v), 2);
    let p = frame(&mut v);
    assert_eq!((px(&p, 20, 10), px(&p, 28, 22)), ([0, 0, 0], [0, 0, 0]));
    // New tools draw with stable ids and their options.
    for (t, option) in [(Tool::Reshape, vector::widget::RESHAPE_RADIUS), (Tool::BlobBrush, vector::widget::BLOB_DIAMETER)] {
        v.set_tool(t);
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1900.0, 900.0))), ..Default::default() };
        let _ = ctx.run(input, |ctx| v.ui(ctx));
        let drawn = v.drawn().to_vec();
        for k in [Tool::Width, Tool::Reshape, Tool::BlobBrush, Tool::PaintBucket] {
            assert!(drawn.contains(&widget::tool(k.key())), "{k:?}");
        }
        assert!(drawn.iter().any(|d| d == option), "{option}");
    }
}

#[test]
fn hostile_network_and_width_input_is_refused_typed() {
    let scratch = Scratch::new("vector3-hostile");
    let mut v = paper(&scratch);
    v.set_vector_tool(tool());
    let rect = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 40.0], false).unwrap();
    v.promote_network(&rect).unwrap();
    let r0 = revision(&v);
    assert!(v.edit_network(&rect, NetworkEdit::Connect { a: 0, b: 99 }).unwrap_err().contains("index_out_of_bounds"));
    assert!(v.edit_network(&rect, NetworkEdit::MoveVertex { vertex: 0, dx: f64::NAN, dy: 0.0 }).is_err());
    assert!(v.edit_network(&rect, NetworkEdit::DeleteSegment(99)).is_err());
    assert!(v.network_paint_bucket(&rect, [f64::INFINITY, 0.0], true).is_err());
    assert!(v.set_width_point(&rect, 1.5, 3.0).is_err());
    assert!(v.set_width_point(&rect, 0.5, -1.0).is_err());
    assert!(v.blob_brush_stroke(&[]).is_err());
    assert!(v.reshape_drag([90.0, 60.0], [91.0, 60.0], 1.0).is_err());
    assert_eq!(revision(&v), r0, "refusals change nothing");
    // Deleting a segment drops the region it bounded and keeps the network valid.
    v.edit_network(&rect, NetworkEdit::DeleteSegment(0)).unwrap();
    assert!(v.vector_network(&rect).unwrap().regions.is_empty());
    // Hostile entry bytes: a network record with an out-of-range index, a width profile out of
    // order; both refused at decode with the Folio / Nib code.
    let doc = v.run().snapshot().unwrap().document().clone();
    let cancel = hsk_studio_accord::CancellationToken::default();
    let entry = v.vector_entry().unwrap();
    let mut wire: serde_json::Value = serde_json::from_slice(&entry.to_bytes(&doc.document_id).unwrap()).unwrap();
    let last = wire["paths"].as_array().unwrap().len() - 1;
    wire["paths"][last]["network"]["segments"][0]["end"] = serde_json::json!(4000);
    assert!(VectorEntry::decode(&serde_json::to_vec(&wire).unwrap(), &doc, &cancel).unwrap_err().contains("DanglingReference"));
    let mut wire: serde_json::Value = serde_json::from_slice(&entry.to_bytes(&doc.document_id).unwrap()).unwrap();
    wire["paths"][0]["width"] = serde_json::json!([[0.6, 1.0], [0.2, 1.0]]);
    assert!(VectorEntry::decode(&serde_json::to_vec(&wire).unwrap(), &doc, &cancel).unwrap_err().contains("width_profile"));
    let mut wire: serde_json::Value = serde_json::from_slice(&entry.to_bytes(&doc.document_id).unwrap()).unwrap();
    wire["paths"][last]["network"]["regions"] = serde_json::json!([{"winding_rule": "NONE", "loops": [[0]], "fills": true}]);
    assert!(VectorEntry::decode(&serde_json::to_vec(&wire).unwrap(), &doc, &cancel).unwrap_err().contains("vector_entry_network"), "a region never carries NONE");
}
