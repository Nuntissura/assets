//! SMB-COMPOSITE-STROKE-GEOMETRY (feature `viewer`): the pen joins the edges of a vector network
//! layer (one transaction per click), and a network layer with fill and stroke paints both exactly
//! (fill row + composite `StrokeGeometry` row, no blend tile).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::edit::Tool;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::{ShapeKind, VectorTool};

const W: usize = 96;
const H: usize = 64;

fn px(rgba: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * W + x) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

#[test]
fn pen_joins_network_edges_and_strokes_paint_exactly() {
    let scratch = Scratch::new("vector4-netpen");
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(W as u32, H as u32, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: false, fill: [255, 0, 0], stroke_on: true, stroke: [0, 0, 255], stroke_width: 2.0, ..VectorTool::default() });
    let rect = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 40.0], false).unwrap();
    v.promote_network(&rect).unwrap();
    v.select(Some(rect.clone()));
    v.set_tool(Tool::Pen);
    let revision = |v: &Viewer| v.run().snapshot().unwrap().document().revision;
    let r0 = revision(&v);
    // Click on the top edge, then on the bottom edge: both edges are split and joined.
    assert_eq!(v.pen_point(30.0, 10.0, None, 1.0).unwrap(), rect);
    assert_eq!(v.pen_point(30.0, 40.0, None, 1.0).unwrap(), rect);
    assert_eq!(revision(&v), r0 + 2, "one transaction per click");
    let net = v.vector_network(&rect).unwrap();
    assert_eq!((net.vertices.len(), net.segments.len()), (6, 7));
    assert_eq!(net.detect_regions(true).unwrap().len(), 2);
    v.pen_end();
    // Fill the left face: red inside, the blue strokes exact on the edges and the divider.
    v.network_paint_bucket(&rect, [20.0, 25.0], true).unwrap();
    v.render_if_dirty();
    let f = v.frame().unwrap().rgba8.clone();
    assert_eq!(px(&f, 20, 25), [255, 0, 0]);
    assert_eq!(px(&f, 40, 25), [255, 255, 255]);
    assert_eq!(px(&f, 30, 25), [0, 0, 255], "divider stroke above the fill");
    assert_eq!(px(&f, 20, 10), [0, 0, 255], "edge stroke above the fill");
    // The row just inside the 2 px edge stroke is pure fill: no tile blur across the stroke edge.
    assert_eq!(px(&f, 20, 11), [255, 0, 0]);
    // Undo removes the paint bucket, then the two pen clicks one by one.
    v.undo().unwrap();
    v.undo().unwrap();
    assert_eq!(v.vector_network(&rect).unwrap().segments.len(), 5);
    v.undo().unwrap();
    assert_eq!(v.vector_network(&rect).unwrap().segments.len(), 4);
    // Hostile clicks change nothing.
    let r1 = revision(&v);
    assert!(v.pen_point(f64::NAN, 1.0, None, 1.0).is_err());
    assert_eq!(revision(&v), r1);
}
