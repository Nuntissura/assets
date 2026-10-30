//! SMB-PS-GRADIENT-BUCKET-1 (feature `viewer`): the raster Gradient tool, the Paint Bucket (flood
//! fill) and Edit > Fill in the window, driven headlessly through the canvas actions the egui tools
//! call (`fill_drag`, `fill_click`) and the headless methods (`fill_gradient`, `flood_fill`,
//! `fill_selection`): one undoable chronicle `tiles` step each, named in the operation log, undo
//! restoring the layer byte-identical; typed refusals on non-pixel layers and hostile input; the
//! vector live-paint bucket unchanged.
#![cfg(feature = "viewer")]

mod common;

use common::{L, Scratch, layered_psd};
use hsk_studio_folio::{Payload, TileRef};
use hsk_studio_harness::view::edit::Tool;
use hsk_studio_harness::view::fill_tools::{self as ft, FillSettings, FillSource};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::ShapeKind;
use hsk_studio_harness::view::{Viewer, widget};
use hsk_studio_pigment::fill_tools::{ColourStop, GradientGeometry, TransparencyStop};

const W: usize = 64;

/// 64x64: "paper" opaque white; "art" above it: a red square x, y 16..40, transparent elsewhere.
fn open_two(label: &str) -> (Scratch, Viewer, String, String) {
    let scratch = Scratch::new(label);
    let path = scratch.0.join("two.psd");
    let n = W * W;
    let inside = |i: usize| (16..40).contains(&(i % W)) && (16..40).contains(&(i / W));
    let art: Vec<(i16, Vec<u8>)> = vec![
        (-1, (0..n).map(|i| if inside(i) { 255 } else { 0 }).collect()),
        (0, vec![255; n]),
        (1, vec![0; n]),
        (2, vec![0; n]),
    ];
    let psd = layered_psd(
        64,
        64,
        &[
            L { name: "paper", rect: [0, 0, 64, 64], channels: vec![(0, vec![255; n]), (1, vec![255; n]), (2, vec![255; n])] },
            L { name: "art", rect: [0, 0, 64, 64], channels: art },
        ],
    );
    std::fs::write(&path, psd).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&path).unwrap();
    let id = |v: &Viewer, name: &str| v.layers().into_iter().find(|r| r.name == name).unwrap_or_else(|| panic!("{}", v.state_json())).layer_id;
    let (paper, art) = (id(&v, "paper"), id(&v, "art"));
    (scratch, v, paper, art)
}

fn frame(v: &mut Viewer) -> Vec<u8> {
    v.render_if_dirty();
    v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).rgba8.clone()
}

fn px(rgba: &[u8], x: usize, y: usize) -> [u8; 4] {
    let i = (y * W + x) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

fn tiles(v: &Viewer, layer: &str) -> Vec<TileRef> {
    let snap = v.run().snapshot().unwrap();
    let l = snap.document().layers.iter().find(|l| l.layer_id == layer).unwrap();
    match &l.payload {
        Payload::Raster { tiles } => tiles.clone(),
        other => panic!("not raster: {other:?}"),
    }
}

fn revision(v: &Viewer) -> u64 {
    v.run().snapshot().unwrap().document().revision
}

fn last_op(v: &Viewer) -> String {
    v.run().ops().last().map(|o| o["op"].as_str().unwrap_or("").to_string()).unwrap_or_default()
}

fn black_to_white() -> FillSettings {
    let colour = vec![ColourStop { location: 0, midpoint: 50.0, colour: [0.0; 3] }, ColourStop { location: 4096, midpoint: 50.0, colour: [1.0; 3] }];
    let alpha = vec![TransparencyStop { location: 0, midpoint: 50.0, opacity: 100.0 }, TransparencyStop { location: 4096, midpoint: 50.0, opacity: 100.0 }];
    FillSettings { stops: Some((colour, alpha)), ..FillSettings::default() }
}

#[test]
fn gradient_drag_is_one_undo_step_and_undo_restores_byte_identical() {
    let (_s, mut v, paper, _art) = open_two("fill-gradient");
    let before_tiles = tiles(&v, &paper);
    let before = frame(&mut v);
    let r0 = revision(&v);
    // The canvas drag of the Gradient tool on the selected layer: black (left) to white (right).
    v.select(Some(paper.clone()));
    v.set_tool(Tool::Gradient);
    v.set_fill_tools(black_to_white());
    v.fill_drag([0.0, 8.0], [64.0, 8.0]).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(revision(&v), r0 + 1, "one chronicle step");
    assert_eq!(last_op(&v), "fill_gradient");
    let after = frame(&mut v);
    // Display-encoded interpolation (Studio reading): column x has level ~ (x + 0.5) / 64 * 255.
    let (l, r) = (px(&after, 2, 4), px(&after, 61, 4));
    println!("gradient: left {l:?} right {r:?}");
    assert!(l[0] < 20 && r[0] > 235, "{l:?} {r:?}");
    assert!((i32::from(px(&after, 32, 4)[0]) - 129).abs() <= 2, "{:?}", px(&after, 32, 4));
    // The art square above the paper is untouched.
    assert_eq!(px(&after, 20, 20), px(&before, 20, 20));
    // Undo: the layer's tiles are byte-identical (same content digests), so are the pixels.
    v.undo().unwrap();
    assert_eq!(tiles(&v, &paper), before_tiles);
    assert_eq!(frame(&mut v), before);
    v.redo().unwrap();
    assert_eq!(frame(&mut v), after);
    // A gradient on an empty new raster layer creates its tiles (foreground to transparent).
    let empty = v.new_raster_layer("empty").unwrap();
    v.select(Some(empty.clone()));
    v.set_fill_tools(FillSettings { geometry: GradientGeometry::Radial, ..FillSettings::default() });
    let r1 = revision(&v);
    let settings = v.fill_tools_settings().clone();
    v.fill_gradient(&empty, [32.0, 32.0], [32.0, 64.0], &settings).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(revision(&v), r1 + 1);
    assert!(!tiles(&v, &empty).is_empty());
    v.undo().unwrap();
    assert!(tiles(&v, &empty).is_empty());
}

#[test]
fn bucket_click_fills_the_region_in_one_undo_step() {
    let (_s, mut v, paper, art) = open_two("fill-bucket");
    let before_tiles = tiles(&v, &art);
    let before = frame(&mut v);
    v.select(Some(art.clone()));
    v.set_tool(Tool::FloodFill);
    let mut brush = v.edit().brush;
    brush.rgb8 = [0, 0, 255];
    v.set_brush(brush);
    v.set_fill_tools(FillSettings { tolerance: 0.0, anti_alias: false, ..FillSettings::default() });
    let r0 = revision(&v);
    // Click inside the red square: the square turns blue, the rest stays as it was.
    v.fill_click(20.5, 20.5).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(revision(&v), r0 + 1);
    assert_eq!(last_op(&v), "flood_fill");
    let after = frame(&mut v);
    assert_eq!(px(&after, 20, 20), [0, 0, 255, 255]);
    assert_eq!(px(&after, 39, 39), [0, 0, 255, 255]);
    for (x, y) in [(5, 5), (15, 20), (40, 20), (60, 60)] {
        assert_eq!(px(&after, x, y), px(&before, x, y), "({x}, {y})");
    }
    v.undo().unwrap();
    assert_eq!(tiles(&v, &art), before_tiles);
    assert_eq!(frame(&mut v), before);
    // Click in the transparent area of the art layer: everything around the square is filled.
    v.fill_click(5.0, 5.0).unwrap_or_else(|e| panic!("{e}"));
    let around = frame(&mut v);
    assert_eq!((px(&around, 5, 5), px(&around, 20, 20)), ([0, 0, 255, 255], [255, 0, 0, 255]));
    // Sample merged reads the composite (red square over white paper), not the selected layer: on
    // the all-white paper the region is the white area around the square only (the paper alone
    // would flood everywhere). Hiding the art layer shows the paper's unpainted square.
    v.undo().unwrap();
    v.select(Some(paper.clone()));
    v.set_fill_tools(FillSettings { tolerance: 0.0, anti_alias: false, sample_merged: true, ..FillSettings::default() });
    v.fill_click(5.0, 5.0).unwrap_or_else(|e| panic!("{e}"));
    v.set_visible(&art, false).unwrap();
    let paper_only = frame(&mut v);
    assert_eq!((px(&paper_only, 5, 5), px(&paper_only, 20, 20)), ([0, 0, 255, 255], [255, 255, 255, 255]));
}

#[test]
fn edit_fill_of_a_selection_and_typed_refusals() {
    let (_s, mut v, paper, art) = open_two("fill-selection");
    let before = frame(&mut v);
    v.select_rect(44.0, 44.0, 10.0, 10.0).unwrap();
    let r0 = revision(&v);
    v.fill_selection(&paper, &FillSettings::default()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(revision(&v), r0 + 1);
    assert_eq!(last_op(&v), "fill_selection");
    let after = frame(&mut v);
    let brush = v.edit().brush.rgb8;
    assert_eq!(px(&after, 48, 48), [brush[0], brush[1], brush[2], 255]);
    // Outside the selection every pixel is byte-identical.
    for y in 0..W {
        for x in 0..W {
            if !((44..54).contains(&x) && (44..54).contains(&y)) {
                assert_eq!(px(&after, x, y), px(&before, x, y), "({x}, {y})");
            }
        }
    }
    v.clear_selection();
    // Non-pixel layers are refused, naming the kind; nothing is rasterized (STU-RAS-170).
    let rect = v.draw_shape(ShapeKind::Rectangle, [4.0, 4.0], [12.0, 12.0], false).unwrap();
    let group = v.group_layers(std::slice::from_ref(&art), "Group").unwrap();
    for layer in [rect.as_str(), group.as_str()] {
        let r = revision(&v);
        let e = v.fill_gradient(layer, [0.0, 0.0], [10.0, 0.0], &FillSettings::default()).unwrap_err();
        assert!(e.contains("layer_not_pixel"), "{e}");
        let e2 = v.flood_fill(layer, 1.0, 1.0, &FillSettings::default()).unwrap_err();
        assert!(e2.contains("layer_not_pixel"), "{e2}");
        assert_eq!(revision(&v), r, "a refusal changes nothing");
    }
    // Hostile input: typed codes, no change.
    let r = revision(&v);
    let cases = [
        (v.fill_gradient(&paper, [f64::NAN, 0.0], [10.0, 0.0], &FillSettings::default()), "nonfinite"),
        (v.fill_gradient(&paper, [3.0, 3.0], [3.0, 3.0], &FillSettings::default()), "zero_length_drag"),
        (v.flood_fill(&paper, -5.0, 3.0, &FillSettings::default()), "seed_outside_canvas"),
        (v.flood_fill(&paper, f64::INFINITY, 3.0, &FillSettings::default()), "nonfinite"),
        (v.flood_fill(&paper, 3.0, 3.0, &FillSettings { content: FillSource::Pattern("no-such-pattern".into()), ..FillSettings::default() }), "pattern_unavailable"),
        (v.fill_selection(&paper, &FillSettings { opacity_pct: 150.0, ..FillSettings::default() }), "opacity_out_of_range"),
        (v.fill_gradient("no-such-layer", [0.0, 0.0], [10.0, 0.0], &FillSettings::default()), "unknown layer"),
    ];
    for (result, code) in cases {
        let e = result.unwrap_err();
        assert!(e.contains(code), "{code}: {e}");
    }
    assert_eq!(revision(&v), r);
    // No resolvable pattern in this document: the window offers none.
    assert!(v.fill_patterns().is_empty());
}

#[test]
fn vector_paint_bucket_is_unchanged_and_one_ui_frame_draws_the_fill_tools() {
    let (_s, mut v, _paper, _art) = open_two("fill-ui");
    // Regression: the Illustrator live-paint bucket keeps its key and behaviour on vector layers.
    assert_eq!(Tool::PaintBucket.key(), "paint_bucket");
    assert!(Tool::PaintBucket.is_vector() && !Tool::Gradient.is_vector() && !Tool::FloodFill.is_vector());
    v.set_tool(Tool::PaintBucket);
    let rect = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [50.0, 40.0], false).unwrap();
    v.promote_network(&rect).unwrap();
    let r = revision(&v);
    v.network_paint_bucket(&rect, [20.0, 25.0], true).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(revision(&v), r + 1);
    // STU-SHL-155 tool ids, the option row (shown while a fill tool is active) and the hover
    // strings.
    assert_eq!((Tool::Gradient.key(), Tool::FloodFill.key()), ("gradient", "flood_fill"));
    v.set_tool(Tool::Gradient);
    let ctx = egui::Context::default();
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0))), ..Default::default() };
    let _ = ctx.run(input, |ctx| v.ui(ctx));
    let drawn = v.drawn().to_vec();
    for tool in [Tool::Gradient, Tool::FloodFill] {
        assert!(Tool::ALL.contains(&tool));
        assert!(drawn.contains(&widget::tool(tool.key())), "{tool:?} not drawn: {drawn:?}");
        assert!(!ft::tool_hover(tool).is_empty());
    }
    for name in [ft::GEOMETRY, ft::MODE, ft::OPACITY, ft::REVERSE, ft::DITHER, ft::TOLERANCE, ft::CONTIGUOUS, ft::ANTI_ALIAS, ft::SAMPLE_MERGED, ft::FILL_SELECTION] {
        assert!(drawn.iter().any(|d| d == name), "{name} not drawn: {drawn:?}");
    }
    let state = v.state_json();
    assert_eq!(state["edit"]["tools"].as_array().unwrap().len(), Tool::ALL.len());
    assert_eq!(state["edit"]["fill"]["geometry"], "linear");
}
