//! SMB-VECTOR-STYLES slice 2 (feature `viewer`): Folio fill and stroke styles authored in the
//! viewer (solid, linear and radial gradient fills; the stroke panel: weight, caps, joins, dash,
//! alignment), swatches (edit updates every use, detach keeps a local copy), network regions
//! painted with a swatch, and width profiles saved as Folio records. Each edit is one undo step;
//! every layer renders through composite's Folio adapter. Expected pixels are hand-derived from the
//! geometry (document px, 1 px = 1 pixel).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_folio::{StrokeAlign, StrokeCap, StrokeJoin};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::edit::Tool;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::{GradientStop, PaintSpec, RowKind, ShapeKind, StrokePanel, VectorEntry, VectorTool};

const W: usize = 96;
const H: usize = 64;
const WHITE: [u8; 3] = [255, 255, 255];
const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];

fn paper(scratch: &Scratch) -> Viewer {
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(W as u32, H as u32, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: true, fill: RED, stroke_on: false, stroke: BLUE, stroke_width: 2.0, ..VectorTool::default() });
    v
}

fn frame(v: &mut Viewer) -> Vec<u8> {
    v.render_if_dirty();
    v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).rgba8.clone()
}

fn px(f: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * W + x) * 4;
    [f[i], f[i + 1], f[i + 2]]
}

fn stops(a: [u8; 3], b: [u8; 3]) -> Vec<GradientStop> {
    vec![GradientStop { position: 0.0, colour: a, opacity: 1.0 }, GradientStop { position: 1.0, colour: b, opacity: 1.0 }]
}

fn panel(weight: f64) -> StrokePanel {
    StrokePanel::solid(BLUE, weight)
}

#[test]
fn fill_styles_are_one_undo_step_each_and_persist() {
    let scratch = Scratch::new("styles-fill");
    let mut v = paper(&scratch);
    let rect = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [80.0, 50.0], false).unwrap();
    let plain = frame(&mut v);
    assert_eq!(px(&plain, 45, 30), RED);
    let revision = v.run().snapshot().unwrap().document().revision;
    // Linear black -> white along x: dark at the left end, light at the right, increasing.
    v.set_fill(&rect, Some(&PaintSpec::Linear { from: [10.0, 30.0], to: [80.0, 30.0], stops: stops([0, 0, 0], WHITE) })).unwrap();
    let linear = frame(&mut v);
    let lum = |x| px(&linear, x, 30)[0];
    assert!(lum(10) < 40 && lum(79) > 215, "{} {}", lum(10), lum(79));
    assert!(lum(20) < lum(40) && lum(40) < lum(60), "monotonic");
    assert_eq!(px(&linear, 40, 30)[0], px(&linear, 40, 30)[2], "grey ramp");
    assert_eq!(v.run().snapshot().unwrap().document().revision, revision, "entry-only step");
    // Radial red -> blue from the centre.
    v.set_fill(&rect, Some(&PaintSpec::Radial { centre: [45.0, 30.0], radius: 30.0, stops: stops(RED, BLUE) })).unwrap();
    let radial = frame(&mut v);
    let c = px(&radial, 45, 30);
    assert!(c[0] > 245 && c[1] == 0 && c[2] < 60, "{c:?}");
    assert!(px(&radial, 74, 30)[2] > 200 && px(&radial, 74, 30)[0] < 120, "{:?}", px(&radial, 74, 30));
    // Solid, then none.
    v.set_fill(&rect, Some(&PaintSpec::Solid(GREEN))).unwrap();
    assert_eq!(px(&frame(&mut v), 45, 30), GREEN);
    v.set_fill(&rect, None).unwrap();
    assert_eq!(px(&frame(&mut v), 45, 30), WHITE);
    // One undo step per edit: back through solid, radial, linear to the plain layer; redo again.
    v.undo().unwrap();
    assert_eq!(px(&frame(&mut v), 45, 30), GREEN);
    v.undo().unwrap();
    assert_eq!(frame(&mut v), radial);
    v.undo().unwrap();
    assert_eq!(frame(&mut v), linear);
    v.undo().unwrap();
    assert_eq!(frame(&mut v), plain);
    v.redo().unwrap();
    assert_eq!(frame(&mut v), linear);
    // Save and reopen: the Folio style and the pixels come back.
    let out = scratch.0.join("styled.handshake");
    v.save(&out, false).unwrap();
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&out).unwrap();
    assert_eq!(again.layer_paint_style(&rect).unwrap(), v.layer_paint_style(&rect).unwrap());
    assert_eq!(frame(&mut again), linear);
}

#[test]
fn stroke_panel_weight_caps_joins_dash_and_alignment() {
    let scratch = Scratch::new("styles-stroke");
    let mut v = paper(&scratch);
    let rect = v.draw_shape(ShapeKind::Rectangle, [20.0, 20.0], [60.0, 50.0], false).unwrap();
    v.set_fill(&rect, None).unwrap();
    // Centre 6 px on the left edge x = 20: x 17..23.
    v.set_stroke(&rect, Some(&panel(6.0))).unwrap();
    let centre = frame(&mut v);
    assert_eq!((px(&centre, 17, 35), px(&centre, 22, 35), px(&centre, 16, 35), px(&centre, 23, 35)), (BLUE, BLUE, WHITE, WHITE));
    // Inside: x 20..26; outside: x 14..20.
    v.set_stroke(&rect, Some(&StrokePanel { align: StrokeAlign::Inside, ..panel(6.0) })).unwrap();
    let inside = frame(&mut v);
    assert_eq!((px(&inside, 18, 35), px(&inside, 25, 35)), (WHITE, BLUE));
    v.set_stroke(&rect, Some(&StrokePanel { align: StrokeAlign::Outside, ..panel(6.0) })).unwrap();
    let outside = frame(&mut v);
    assert_eq!((px(&outside, 15, 35), px(&outside, 21, 35)), (BLUE, WHITE));
    // Joins at the corner (20, 20), weight 10: miter fills the outer square, bevel cuts it.
    v.set_stroke(&rect, Some(&StrokePanel { join: StrokeJoin::Miter, ..panel(10.0) })).unwrap();
    assert_eq!(px(&frame(&mut v), 15, 15), BLUE);
    v.set_stroke(&rect, Some(&StrokePanel { join: StrokeJoin::Bevel, ..panel(10.0) })).unwrap();
    assert_eq!(px(&frame(&mut v), 15, 15), WHITE);
    // Caps and dashes on an open line from (10, 58) to (90, 58), weight 10 (radius 5).
    let line = v.draw_shape(ShapeKind::Line, [10.0, 58.0], [90.0, 58.0], false).unwrap();
    v.set_stroke(&line, Some(&StrokePanel { cap: StrokeCap::Butt, ..panel(10.0) })).unwrap();
    assert_eq!(px(&frame(&mut v), 7, 58), WHITE);
    v.set_stroke(&line, Some(&StrokePanel { cap: StrokeCap::Round, ..panel(10.0) })).unwrap();
    let round = frame(&mut v);
    assert_eq!(px(&round, 7, 58), BLUE);
    assert_ne!(px(&round, 6, 62), BLUE, "round cap leaves the square corner");
    v.set_stroke(&line, Some(&StrokePanel { cap: StrokeCap::Projecting, ..panel(10.0) })).unwrap();
    assert_eq!(px(&frame(&mut v), 6, 62), BLUE);
    v.set_stroke(&line, Some(&StrokePanel { dash: vec![10.0, 10.0], ..panel(4.0) })).unwrap();
    let dashed = frame(&mut v);
    assert_eq!((px(&dashed, 15, 58), px(&dashed, 25, 58), px(&dashed, 35, 58)), (BLUE, WHITE, BLUE));
    // One step each: undo the dash restores the projecting cap frame's dash-free line.
    v.undo().unwrap();
    assert_eq!(px(&frame(&mut v), 25, 58), BLUE);
    // Hostile panels are refused typed and change nothing.
    let before = frame(&mut v);
    for bad in [StrokePanel { weight: f64::NAN, ..panel(1.0) }, StrokePanel { weight: 1.0e9, ..panel(1.0) }, StrokePanel { dash: vec![1.0], ..panel(1.0) }, StrokePanel { miter_limit: 0.5, ..panel(1.0) }] {
        assert!(v.set_stroke(&line, Some(&bad)).is_err());
    }
    assert_eq!(frame(&mut v), before);
}

#[test]
fn swatches_update_every_use_in_one_step_and_detach_keeps_a_copy() {
    let scratch = Scratch::new("styles-swatch");
    let mut v = paper(&scratch);
    let a = v.draw_shape(ShapeKind::Rectangle, [5.0, 5.0], [40.0, 40.0], false).unwrap();
    let b = v.draw_shape(ShapeKind::Rectangle, [50.0, 5.0], [90.0, 40.0], false).unwrap();
    let swatch = v.add_swatch("Swatches/brand", &PaintSpec::Solid(GREEN)).unwrap();
    assert_eq!(v.swatches().unwrap(), vec![(swatch.clone(), "Swatches/brand".to_string())]);
    v.apply_swatch(&a, &swatch, RowKind::Fill).unwrap();
    v.apply_swatch(&b, &swatch, RowKind::Fill).unwrap();
    v.apply_swatch(&a, &swatch, RowKind::Stroke).unwrap();
    let f = frame(&mut v);
    assert_eq!((px(&f, 20, 20), px(&f, 70, 20)), (GREEN, GREEN));
    // Edit: every linked row follows, as one step.
    v.edit_swatch(&swatch, &PaintSpec::Solid(BLUE)).unwrap();
    let f = frame(&mut v);
    assert_eq!((px(&f, 20, 20), px(&f, 70, 20), px(&f, 5, 20)), (BLUE, BLUE, BLUE));
    v.undo().unwrap();
    let f = frame(&mut v);
    assert_eq!((px(&f, 20, 20), px(&f, 70, 20)), (GREEN, GREEN));
    v.redo().unwrap();
    // Detach b: it keeps its blue copy while the swatch moves on.
    v.detach_swatch(&b, RowKind::Fill).unwrap();
    v.edit_swatch(&swatch, &PaintSpec::Solid(RED)).unwrap();
    let f = frame(&mut v);
    assert_eq!((px(&f, 20, 20), px(&f, 70, 20)), (RED, BLUE));
    // Hostile input: unknown swatch, bad gradients; nothing changes.
    assert!(v.apply_swatch(&a, "SSTY-019abcde-0000-7000-8000-00000000ffff", RowKind::Fill).is_err());
    assert!(v.set_fill(&a, Some(&PaintSpec::Linear { from: [0.0, 0.0], to: [f64::NAN, 0.0], stops: stops(RED, BLUE) })).is_err());
    assert!(v.set_fill(&a, Some(&PaintSpec::Radial { centre: [0.0, 0.0], radius: 5.0, stops: stops(RED, BLUE)[..1].to_vec() })).is_err());
    assert!(v.add_swatch("", &PaintSpec::Solid(RED)).is_err());
    assert_eq!(frame(&mut v), f);
}

#[test]
fn network_regions_take_swatches_and_width_profiles_save_as_folio_records() {
    let scratch = Scratch::new("styles-network");
    let mut v = paper(&scratch);
    v.set_vector_tool(VectorTool { fill_on: false, fill: RED, stroke_on: true, stroke: BLUE, stroke_width: 2.0, ..VectorTool::default() });
    let rect = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 40.0], false).unwrap();
    // An unstyled document's entry re-encodes to the same bytes.
    let bytes = v.run().document_entry("vector").unwrap().to_vec();
    let doc = v.run().snapshot().unwrap().document().clone();
    let decoded = VectorEntry::decode(&bytes, &doc, &Default::default()).unwrap();
    assert_eq!(decoded.to_bytes(&doc.document_id).unwrap(), bytes);
    v.promote_network(&rect).unwrap();
    v.select(Some(rect.clone()));
    v.set_tool(Tool::Pen);
    v.pen_point(30.0, 10.0, None, 1.0).unwrap();
    v.pen_point(30.0, 40.0, None, 1.0).unwrap();
    v.pen_end();
    v.network_paint_bucket(&rect, [20.0, 25.0], true).unwrap();
    let swatch = v.add_swatch("Swatches/region", &PaintSpec::Solid(GREEN)).unwrap();
    v.network_region_swatch(&rect, [40.0, 25.0], Some(&swatch)).unwrap();
    let f = frame(&mut v);
    assert_eq!((px(&f, 20, 25), px(&f, 40, 25), px(&f, 30, 25)), (RED, GREEN, BLUE), "layer fill, region swatch, stroke above");
    // The region names the swatch: editing the swatch repaints it.
    v.edit_swatch(&swatch, &PaintSpec::Solid([255, 255, 0])).unwrap();
    assert_eq!(px(&frame(&mut v), 40, 25), [255, 255, 0]);
    // Unfilling the left face keeps the right face's swatch.
    v.network_paint_bucket(&rect, [20.0, 25.0], false).unwrap();
    let f = frame(&mut v);
    assert_eq!((px(&f, 20, 25), px(&f, 40, 25)), (WHITE, [255, 255, 0]));
    // A width profile on an open line: thick in the middle, thin at the ends.
    v.set_vector_tool(VectorTool { fill_on: false, stroke_on: true, stroke: BLUE, stroke_width: 2.0, ..VectorTool::default() });
    let line = v.draw_shape(ShapeKind::Line, [60.0, 50.0], [90.0, 50.0], false).unwrap();
    v.set_width_point(&line, 0.5, 10.0).unwrap();
    let drawn = frame(&mut v);
    assert_eq!((px(&drawn, 75, 53), px(&drawn, 61, 53)), (BLUE, WHITE));
    // Saved as Folio records: the reopened entry holds one width profile, and the pixels match.
    let out = scratch.0.join("network-styles.handshake");
    v.save(&out, false).unwrap();
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&out).unwrap();
    let entry = again.vector_entry().unwrap();
    let records = entry.folio_records(&doc.document_id);
    assert_eq!(records.width_profiles.len(), 1);
    assert_eq!(records.width_profiles[0].points.len(), 3);
    assert_eq!(frame(&mut again), drawn);
}
