//! Lane SMB-FIG-GRID-2 slices 3 and 4 (feature `viewer`): auto layout in the window. Commands on a
//! frame (a group layer whose bottom-most rectangle encloses its other children: its background)
//! edit the design records of the vector entry, re-solve the frame with the components solver and
//! move its children, as ONE undo step; refusals (Folio, solver, unsupported resize) change nothing;
//! the Properties section's `design.auto_layout.*` controls drive the same commands headlessly.
//! Expected positions are hand-computed from the Studio grid reading (lane SMB-FIG-GRID-1; Figma
//! grid behaviour UNVERIFIED, D5). Document px, the frame at (10, 10) and 100 x 80.
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_components::GridTrackMove;
use hsk_studio_folio::{ChildSizing, LayoutMode, StudioGridChild, StudioGridTrackSize};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::design::auto_layout::widget;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::{PaintSpec, ShapeKind, VectorTool};

fn paper(scratch: &Scratch) -> Viewer {
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(160, 120, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: true, fill: [200, 0, 0], stroke_on: false, ..VectorTool::default() });
    v
}

/// The frame: background [10,10]-[110,90], a 10x10, b 20x10, c 10x20 (and with `ellipse` an
/// ellipse d 10x10); returns (frame, [a, b, c, d?]).
fn frame(v: &mut Viewer, ellipse: bool) -> (String, Vec<String>) {
    let bg = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [110.0, 90.0], false).unwrap();
    let mut kids = vec![
        v.draw_shape(ShapeKind::Rectangle, [20.0, 20.0], [30.0, 30.0], false).unwrap(),
        v.draw_shape(ShapeKind::Rectangle, [40.0, 20.0], [60.0, 30.0], false).unwrap(),
        v.draw_shape(ShapeKind::Rectangle, [20.0, 40.0], [30.0, 60.0], false).unwrap(),
    ];
    if ellipse {
        kids.push(v.draw_shape(ShapeKind::Ellipse, [70.0, 20.0], [80.0, 30.0], false).unwrap());
    }
    let mut all = vec![bg];
    all.extend(kids.iter().cloned());
    let g = v.group_layers(&all, "frame").unwrap();
    (g, kids)
}

fn bounds(v: &Viewer, layer: &str) -> [f64; 4] {
    let b = v.layer_bounds(layer).unwrap();
    [b.min[0], b.min[1], b.max[0], b.max[1]]
}

fn near(v: &Viewer, layer: &str, e: [f64; 4]) {
    let a = bounds(v, layer);
    assert!(a.iter().zip(e).all(|(x, y)| (x - y).abs() < 1e-6), "{layer}: {a:?} != {e:?}\n{}", v.state_json()["status"]);
}

/// The document's layers and its vector entry bytes (what an undo must restore).
fn snapshot(v: &Viewer) -> (String, Vec<u8>) {
    let doc = v.run().snapshot().unwrap().document();
    (serde_json::to_string(&doc.layers).unwrap(), v.run().document_entry("vector").map(<[u8]>::to_vec).unwrap_or_default())
}

fn revision(v: &Viewer) -> u64 {
    v.run().snapshot().unwrap().document().revision
}

#[test]
fn switching_a_three_child_frame_to_grid_places_the_cells_in_one_undo_step() {
    let scratch = Scratch::new("al-grid");
    let mut v = paper(&scratch);
    let (g, k) = frame(&mut v, false);
    let before = snapshot(&v);
    // Three in-flow children: a 2 x 2 HUG grid; columns 10 (a, c) and 20 (b), rows 10 (a, b) and
    // 20 (c); ROW_AUTO_FLOW fills (0,0), (0,1), (1,0) from the frame's top-left (10, 10).
    v.set_auto_layout_mode(&g, LayoutMode::Grid).unwrap();
    near(&v, &k[0], [10.0, 10.0, 20.0, 20.0]);
    near(&v, &k[1], [20.0, 10.0, 40.0, 20.0]);
    near(&v, &k[2], [10.0, 20.0, 20.0, 40.0]);
    v.select(Some(g.clone()));
    let state = v.state_json();
    let a = &state["design"]["auto_layout"]["selected"]["auto_layout"];
    assert_eq!(a["layout_mode"], "GRID", "{state}");
    assert_eq!((a["grid"]["grid_row_count"].as_u64(), a["grid"]["grid_column_count"].as_u64()), (Some(2), Some(2)));
    // One undo restores the layers and the entry byte for byte.
    v.undo().unwrap();
    assert_eq!(snapshot(&v), before);
    v.redo().unwrap();
    near(&v, &k[1], [20.0, 10.0, 40.0, 20.0]);
}

#[test]
fn a_flex_column_and_gaps_recompute_the_positions() {
    let scratch = Scratch::new("al-flex");
    let mut v = paper(&scratch);
    let (g, k) = frame(&mut v, false);
    v.set_auto_layout_mode(&g, LayoutMode::Grid).unwrap();
    // FIXED 30 + FLEX 1 in a fixed 100 px frame: the FLEX column takes 70, b starts at 10 + 30.
    let hug = StudioGridTrackSize::hug();
    v.set_grid_tracks(&g, &[hug, hug], &[StudioGridTrackSize::fixed(30.0), StudioGridTrackSize::flex(1.0)]).unwrap();
    near(&v, &k[0], [10.0, 10.0, 20.0, 20.0]);
    near(&v, &k[1], [40.0, 10.0, 60.0, 20.0]);
    // Gaps 5 (rows) and 10 (columns): b at 10 + 30 + 10, c at 10 + 10 + 5.
    v.set_grid_gaps(&g, 5.0, 10.0).unwrap();
    near(&v, &k[1], [50.0, 10.0, 70.0, 20.0]);
    near(&v, &k[2], [10.0, 25.0, 20.0, 45.0]);
}

#[test]
fn a_span_past_the_count_is_refused_by_folio_and_changes_nothing() {
    let scratch = Scratch::new("al-span");
    let mut v = paper(&scratch);
    let (g, k) = frame(&mut v, false);
    v.set_auto_layout_mode(&g, LayoutMode::Grid).unwrap();
    let (before, rev) = (snapshot(&v), revision(&v));
    let e = v.set_grid_child(&k[2], Some(StudioGridChild { grid_row_anchor_index: 1, grid_column_anchor_index: 1, grid_row_span: 1, grid_column_span: 2 })).unwrap_err();
    assert!(e.contains("folio") && e.contains("grid_column_span"), "{e}");
    assert_eq!((snapshot(&v), revision(&v)), (before, rev));
}

#[test]
fn reordering_rows_moves_the_anchored_children_with_their_rows() {
    let scratch = Scratch::new("al-reorder");
    let mut v = paper(&scratch);
    let (g, k) = frame(&mut v, false);
    v.set_auto_layout_mode(&g, LayoutMode::Grid).unwrap();
    v.grid_append_child_at(&g, &k[0], 0, 0).unwrap();
    v.grid_append_child_at(&g, &k[2], 1, 0).unwrap();
    near(&v, &k[2], [10.0, 20.0, 20.0, 40.0]);
    // Row 0 -> 1: c (20 high) now anchors row 0, a row 1; b auto-flows to the first free cell
    // (0, 1). Rows: 20 (c, b) and 10 (a); columns: 10 (a, c) and 20 (b).
    v.grid_reorder_rows(&g, GridTrackMove { from_index: 0, to_index: 1 }).unwrap();
    near(&v, &k[2], [10.0, 10.0, 20.0, 30.0]);
    near(&v, &k[1], [20.0, 10.0, 40.0, 20.0]);
    near(&v, &k[0], [10.0, 30.0, 20.0, 40.0]);
}

#[test]
fn a_fill_rectangle_is_resized_and_a_fill_path_is_refused() {
    let scratch = Scratch::new("al-fill");
    let mut v = paper(&scratch);
    let (g, k) = frame(&mut v, true);
    v.set_auto_layout_mode(&g, LayoutMode::Grid).unwrap();
    // 2 x 2: a (0,0), b (0,1), c (1,0), d (1,1); column 1 FIXED 40.
    let hug = StudioGridTrackSize::hug();
    v.set_grid_tracks(&g, &[hug, hug], &[hug, StudioGridTrackSize::fixed(40.0)]).unwrap();
    v.set_child_sizing(&k[1], ChildSizing::Fill, ChildSizing::Fixed).unwrap();
    near(&v, &k[1], [20.0, 10.0, 60.0, 20.0]);
    // The ellipse is a path: a FILL size change is refused typed, nothing changes.
    let (before, rev) = (snapshot(&v), revision(&v));
    let e = v.set_child_sizing(&k[3], ChildSizing::Fill, ChildSizing::Fixed).unwrap_err();
    assert!(e.contains("auto_layout_resize_unsupported:vector_path"), "{e}");
    assert_eq!((snapshot(&v), revision(&v)), (before, rev));
}

// ---- slice 4: the Properties section ---------------------------------------------------------

fn ui_frame(v: &mut Viewer, ctx: &egui::Context) {
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 1200.0))), ..Default::default() };
    for _ in 0..3 {
        let _ = ctx.run(input.clone(), |ctx| v.ui(ctx));
    }
}

fn drew(v: &Viewer, name: &str) -> bool {
    v.drawn().iter().any(|d| d == name)
}

#[test]
fn the_section_ids_drive_the_commands_and_a_frame_without_records_has_no_section() {
    let scratch = Scratch::new("al-section");
    let mut v = paper(&scratch);
    let (g, k) = frame(&mut v, false);
    let ctx = egui::Context::default();
    v.select(Some(g.clone()));
    ui_frame(&mut v, &ctx);
    assert!(!drew(&v, widget::SECTION), "no design records: no section");
    assert!(v.auto_layout_control(&widget::mode("GRID"), "").is_err());
    // With records (here from the command surface) the frame shows its controls.
    v.set_auto_layout_mode(&g, LayoutMode::Horizontal).unwrap();
    ui_frame(&mut v, &ctx);
    for id in [widget::SECTION.to_owned(), widget::mode("GRID"), widget::SPACING.to_owned(), widget::padding("left")] {
        assert!(drew(&v, &id), "{id}: {:?}", v.drawn());
    }
    // The ids run the same commands: GRID, then a 10 px column gap (b at 10 + 10 + 10).
    v.auto_layout_control(&widget::mode("GRID"), "").unwrap();
    v.auto_layout_control(widget::COLUMN_GAP, "10").unwrap();
    near(&v, &k[1], [30.0, 10.0, 50.0, 20.0]);
    ui_frame(&mut v, &ctx);
    for id in [widget::ROWS.to_owned(), widget::track("column", 1, "type"), widget::track("row", 0, "later"), widget::AUTO_TRACKS.to_owned(), widget::ITEMS_POSITIONING.to_owned()] {
        assert!(drew(&v, &id), "{id}: {:?}", v.drawn());
    }
    // A count past Folio's bound is Folio's typed refusal; nothing is clamped or changed.
    let rev = revision(&v);
    let e = v.auto_layout_control(widget::ROWS, "1001").unwrap_err();
    assert!(e.contains("grid_row_count"), "{e}");
    assert_eq!(revision(&v), rev);
    // A FIXED 30 first column through its type and value controls.
    v.auto_layout_control(&widget::track("column", 0, "type"), "FIXED").unwrap();
    v.auto_layout_control(&widget::track("column", 0, "value"), "30").unwrap();
    near(&v, &k[1], [50.0, 10.0, 70.0, 20.0]);
    // A grid child shows its anchor controls; anchoring c at (0, 1) moves it there.
    v.select(Some(k[2].clone()));
    ui_frame(&mut v, &ctx);
    assert!(drew(&v, widget::CHILD_ANCHORED), "{:?}", v.drawn());
    v.auto_layout_control(widget::CHILD_ANCHORED, "true").unwrap();
    v.auto_layout_control(&widget::child("column"), "1").unwrap();
    assert_eq!(v.state_json()["design"]["auto_layout"]["selected"]["child_layout"]["grid_child"]["grid_column_anchor_index"], 1);
    ui_frame(&mut v, &ctx);
    assert!(drew(&v, &widget::child("row_span")), "{:?}", v.drawn());
}

// ---- gap closures: restyle and import ----------------------------------------------------------

#[test]
fn restyling_a_grid_child_keeps_its_anchor() {
    let scratch = Scratch::new("al-restyle");
    let mut v = paper(&scratch);
    let (g, k) = frame(&mut v, false);
    v.set_auto_layout_mode(&g, LayoutMode::Grid).unwrap();
    v.grid_append_child_at(&g, &k[1], 1, 1).unwrap();
    let anchor = |v: &mut Viewer| {
        v.select(Some(k[1].clone()));
        v.state_json()["design"]["auto_layout"]["selected"]["child_layout"]["grid_child"].clone()
    };
    let before = anchor(&mut v);
    assert_eq!((before["grid_row_anchor_index"].as_u64(), before["grid_column_anchor_index"].as_u64()), (Some(1), Some(1)));
    // A fill change and a graphic style both rewrite the layer's style reference only.
    v.set_fill(&k[1], Some(&PaintSpec::Solid([0, 0, 255]))).unwrap();
    assert_eq!(anchor(&mut v), before);
    let style = v.add_graphic_style("blue", &k[1]).unwrap();
    v.apply_graphic_style(&k[0], &style).unwrap();
    v.apply_graphic_style(&k[1], &style).unwrap();
    assert_eq!(anchor(&mut v), before);
    // The records still solve: b stays in cell (1, 1) after a gap change (a and c auto-flow to
    // (0, 0) and (0, 1); columns 10 and 20, rows 20 (c) and 10 (b), row gap 5: b at (20, 35)).
    v.set_grid_gaps(&g, 5.0, 0.0).unwrap();
    near(&v, &k[1], [20.0, 35.0, 40.0, 45.0]);
}

#[test]
fn an_imported_flow_frame_shows_its_records_in_the_section() {
    let scratch = Scratch::new("al-import");
    let fig = scratch.0.join("fig-components-autolayout.fig");
    std::fs::copy(format!("{}/../hsk-studio-interop-fig/tests/fixtures/fig-components-autolayout.fig", env!("CARGO_MANIFEST_DIR")), &fig).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.import_fig(&fig, &scratch.0.join("imported.handshake"), false).unwrap();
    let frame = v.run().snapshot().unwrap().document().layers.iter().find(|l| l.name == "fixture-button" && l.kind == "group").expect("fixture-button frame").layer_id.clone();
    // The imported HORIZONTAL record (spacing 24, padding 24) rides in the vector entry.
    let view = v.auto_layout_view(&frame).expect("section");
    let a = view.auto_layout.expect("auto layout");
    assert_eq!((a.layout_mode, a.item_spacing, a.padding_left, a.padding_top), (LayoutMode::Horizontal, 24.0, 24.0, 24.0));
    v.select(Some(frame.clone()));
    let ctx = egui::Context::default();
    ui_frame(&mut v, &ctx);
    assert!(drew(&v, widget::SPACING) && drew(&v, &widget::mode("GRID")), "{:?}", v.drawn());
    // Its components hold text layers, whose positions live in the text entry: a re-layout is a
    // typed refusal and changes nothing (one undo step carries one entry).
    let rev = revision(&v);
    let e = v.set_auto_layout_spacing(&frame, 30.0, [24.0; 4]).unwrap_err();
    assert!(e.contains("auto_layout_text_layer_unsupported"), "{e}");
    assert_eq!(revision(&v), rev);
}
