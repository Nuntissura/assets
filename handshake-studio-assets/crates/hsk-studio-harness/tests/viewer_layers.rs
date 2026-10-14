//! SMB-HARNESS-UI-3 (feature `viewer`): layer management through chronicle `placement`
//! transactions with undo (new raster layer, duplicate, delete, reorder, rename, group, ungroup),
//! painting into empty areas creates tiles, and a history that undoes every step. Driven
//! headlessly through the `Viewer` methods the layer panel buttons call (no window, CX-SAFE-002).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_folio::Payload;
use hsk_studio_harness::view::edit::BrushSettings;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::{Viewer, widget};
use hsk_studio_pigment::brush::StrokeSample;

fn pixel(v: &mut Viewer, x: usize, y: usize) -> [u8; 4] {
    v.render_if_dirty();
    let f = v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json()));
    let i = (y * f.width as usize + x) * 4;
    f.rgba8[i..i + 4].try_into().unwrap()
}

fn close(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1)
}

/// Layer panel rows, top-first, as "name" or "  name" per depth.
fn rows(v: &Viewer) -> Vec<String> {
    v.layers().iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.name)).collect()
}

fn tiles(v: &Viewer, id: &str) -> usize {
    let snap = v.run().snapshot().unwrap();
    match &snap.document().layers.iter().find(|l| l.layer_id == id).unwrap().payload {
        Payload::Raster { tiles } => tiles.len(),
        _ => panic!("not raster"),
    }
}

fn line(y: f64, x0: f64, x1: f64) -> Vec<StrokeSample> {
    [x0, x1].iter().map(|&x| StrokeSample { x, y, pressure: Some(1.0), time_ms: None }).collect()
}

const WHITE: [u8; 4] = [255, 255, 255, 255];
const RED: [u8; 4] = [255, 0, 0, 255];

fn open_paper(scratch: &Scratch) -> (Viewer, String) {
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(32, 32, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    let paper = v.layers()[0].layer_id.clone();
    (v, paper)
}

#[test]
fn layer_management_through_chronicle_with_undo_and_save() {
    let scratch = Scratch::new("layers");
    let (mut v, paper) = open_paper(&scratch);
    assert_eq!(rows(&v), ["paper"]);

    // New raster layer on top: no tiles until painted; painting into the empty area creates one.
    let ink = v.new_raster_layer("ink").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(rows(&v), ["ink", "paper"]);
    assert_eq!(tiles(&v, &ink), 0);
    v.set_brush(BrushSettings { diameter_px: 8.0, hardness_pct: 100.0, spacing_pct: 10.0, opacity_pct: 100.0, rgb8: [255, 0, 0] });
    v.paint(&ink, &line(16.0, 0.0, 32.0)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(tiles(&v, &ink), 1, "painting created one 256-pixel cell");
    assert!(close(pixel(&mut v, 4, 16), RED));
    assert!(close(pixel(&mut v, 4, 4), WHITE), "the new tile is transparent outside the stroke");

    // Duplicate (above, new id, same tile bodies), delete the original: still red.
    let copy = v.duplicate_layer(&ink).unwrap_or_else(|e| panic!("{e}"));
    assert_ne!(copy, ink);
    assert_eq!(rows(&v), ["ink copy", "ink", "paper"]);
    v.delete_layer(&ink).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(rows(&v), ["ink copy", "paper"]);
    assert!(close(pixel(&mut v, 4, 16), RED));

    // Reorder below the opaque paper: white.
    v.shift_layer(&copy, false).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(rows(&v), ["paper", "ink copy"]);
    assert!(close(pixel(&mut v, 4, 16), WHITE));
    assert!(v.shift_layer(&copy, false).is_err(), "already at the bottom");

    // Group and ungroup; rename.
    let group = v.group_layers(std::slice::from_ref(&copy), "Group").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(rows(&v), ["paper", "Group", "  ink copy"]);
    v.ungroup(&group).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(rows(&v), ["paper", "ink copy"]);
    v.rename_layer(&paper, "sheet").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(rows(&v), ["sheet", "ink copy"]);

    // Every step undoes, in order, back to the opened document; then every step redoes.
    let steps = v.run().history()["undo"].as_array().unwrap().len();
    assert_eq!(steps, 8, "{}", v.run().history());
    let mut seen = Vec::new();
    for _ in 0..steps {
        v.undo().unwrap_or_else(|e| panic!("{e}"));
        seen.push(rows(&v));
    }
    assert_eq!(seen[1], ["paper", "Group", "  ink copy"], "undo ungroup");
    assert_eq!(seen[4], ["ink copy", "ink", "paper"], "undo delete re-creates the layer");
    assert_eq!(rows(&v), ["paper"]);
    assert!(close(pixel(&mut v, 4, 16), WHITE));
    assert!(v.undo().is_err(), "nothing left to undo");
    for _ in 0..steps {
        v.redo().unwrap_or_else(|e| panic!("{e}"));
    }
    assert_eq!(rows(&v), ["sheet", "ink copy"]);
    v.shift_layer(&copy, true).unwrap();
    assert!(close(pixel(&mut v, 4, 16), RED));

    // A group deleted with its contents comes back whole.
    let g = v.group_layers(std::slice::from_ref(&copy), "Group").unwrap();
    v.delete_layer(&g).unwrap();
    assert_eq!(rows(&v), ["sheet"]);
    v.undo().unwrap();
    assert_eq!(rows(&v), ["Group", "  ink copy", "sheet"]);

    // Save and reopen independently: structure and the painted (created) tile persist.
    let out = scratch.0.join("layers.handshake");
    v.save(&out, false).unwrap();
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&out).unwrap();
    assert_eq!(rows(&again), ["Group", "  ink copy", "sheet"]);
    assert!(close(pixel(&mut again, 4, 16), RED));
    assert!(close(pixel(&mut again, 4, 4), WHITE));

    // Typed refusals change nothing.
    let before = v.run().snapshot().unwrap().encoded_bytes().to_vec();
    assert!(v.move_layer(&copy, Some(&paper), 0).is_err(), "a raster layer is not a stack");
    assert!(v.move_layer(&g, Some(&copy), 0).is_err(), "not into a raster layer");
    assert!(v.duplicate_layer("SLYR-00000000-0000-7000-8000-000000000000").is_err());
    assert_eq!(v.run().snapshot().unwrap().encoded_bytes(), before.as_slice());
}

#[test]
fn multi_step_history_undoes_every_step_and_panel_buttons_act() {
    let scratch = Scratch::new("layers-history");
    let (mut v, paper) = open_paper(&scratch);
    // Two edits of one property: both undo (the UI-2 history replayed stale revisions).
    v.set_opacity(&paper, 50.0).unwrap();
    v.set_opacity(&paper, 70.0).unwrap();
    v.undo().unwrap_or_else(|e| panic!("first undo: {e}"));
    v.undo().unwrap_or_else(|e| panic!("second undo: {e}"));
    assert_eq!(v.layers()[0].opacity, 100.0);
    v.redo().unwrap();
    v.redo().unwrap();
    assert_eq!(v.layers()[0].opacity, 70.0);

    // The layer panel buttons drive the same methods; one UI frame draws their ids.
    v.select(Some(paper.clone()));
    v.layer_action(widget::LAYER_NEW);
    assert_eq!(rows(&v), ["Layer", "paper"]);
    v.layer_action(widget::LAYER_DUPLICATE);
    v.layer_action(widget::LAYER_DOWN);
    assert_eq!(rows(&v), ["Layer", "Layer copy", "paper"]);
    v.layer_action(widget::LAYER_GROUP);
    v.layer_action(widget::LAYER_UNGROUP);
    v.layer_action(widget::LAYER_DELETE);
    assert_eq!(rows(&v).len(), 2, "{:?}", rows(&v));
    let ctx = egui::Context::default();
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))), ..Default::default() };
    let _ = ctx.run(input, |ctx| v.ui(ctx));
    for name in [widget::LAYER_NEW, widget::LAYER_DUPLICATE, widget::LAYER_DELETE, widget::LAYER_UP, widget::LAYER_DOWN, widget::LAYER_GROUP, widget::LAYER_UNGROUP] {
        assert!(v.drawn().iter().any(|d| d == name), "{name} not drawn");
    }
}
