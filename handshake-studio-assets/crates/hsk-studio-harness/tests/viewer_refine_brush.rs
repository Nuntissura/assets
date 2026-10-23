//! Lane SMB-PS-SMART-FILTER-WIRING (feature `viewer`): the refine-edge brush tool
//! (`tool.refine_edge_brush`, STU-RAS-122) drives pigment's refinement surface from the viewer:
//! each stroke is one selection-history step; foreground / background / add / subtract / feather
//! act where painted; the tool and its options are drawn in one UI frame.
#![cfg(feature = "viewer")]

mod common;

use common::{L, Scratch, layered_psd};
use hsk_studio_harness::view::edit::Tool;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::select_liquify::{self as ps, PsSettings};
use hsk_studio_harness::view::{Viewer, widget};
use hsk_studio_pigment::brush::StrokeSample;
use hsk_studio_pigment::select_tools::refine_brush::RefineBrushMode;

/// 64x64 opaque "art": white with a red square x, y 16..40.
fn open_art(label: &str) -> (Scratch, Viewer) {
    let scratch = Scratch::new(label);
    let path = scratch.0.join("art.psd");
    let n = 64 * 64;
    let red = |i: usize| (16..40).contains(&(i % 64)) && (16..40).contains(&(i / 64));
    let ch = |c: u8| -> Vec<u8> { (0..n).map(|i| if red(i) && c != 0 { 0 } else { 255 }).collect() };
    let psd = layered_psd(64, 64, &[L { name: "art", rect: [0, 0, 64, 64], channels: vec![(0, ch(0)), (1, ch(1)), (2, ch(2))] }]);
    std::fs::write(&path, psd).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&path).unwrap();
    let id = v.layers().into_iter().find(|r| r.name == "art").unwrap().layer_id;
    v.select(Some(id));
    (scratch, v)
}

fn s(x: f64, y: f64) -> StrokeSample {
    StrokeSample { x, y, pressure: Some(1.0), time_ms: None }
}

fn mode(v: &mut Viewer, m: RefineBrushMode) {
    v.set_ps(PsSettings { refine_mode: m, refine_size: 6.0, ..PsSettings::default() });
}

#[test]
fn refine_brush_strokes_edit_the_selection_one_step_each() {
    let (_s, mut v) = open_art("refine-brush");
    let refuse = v.ps_gesture(Tool::RefineBrush, &[s(10.0, 10.0)]).unwrap_err();
    assert!(refuse.contains("make a selection first"), "{refuse}");
    v.select_rect(16.0, 16.0, 24.0, 24.0).unwrap();
    // Foreground outside the selection pins it in.
    mode(&mut v, RefineBrushMode::Foreground);
    v.ps_gesture(Tool::RefineBrush, &[s(50.5, 50.5)]).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(v.selection().unwrap().at(50, 50), u16::MAX);
    assert_eq!(v.selection_steps(), 1);
    // Background inside pins it out; the foreground mark of the same surface stays.
    mode(&mut v, RefineBrushMode::Background);
    v.ps_gesture(Tool::RefineBrush, &[s(28.5, 28.5)]).unwrap_or_else(|e| panic!("{e}"));
    let sel = v.selection().unwrap().clone();
    assert_eq!(sel.at(28, 28), 0);
    assert_eq!(sel.at(50, 50), u16::MAX);
    assert_eq!(sel.at(20, 20), u16::MAX);
    // Add and subtract change the selection itself.
    mode(&mut v, RefineBrushMode::AddToSelection);
    v.ps_gesture(Tool::RefineBrush, &[s(8.5, 8.5)]).unwrap();
    assert_eq!(v.selection().unwrap().at(8, 8), u16::MAX);
    mode(&mut v, RefineBrushMode::SubtractFromSelection);
    v.ps_gesture(Tool::RefineBrush, &[s(36.5, 20.5)]).unwrap();
    assert_eq!(v.selection().unwrap().at(36, 20), 0);
    // Feather softens under its stroke only.
    mode(&mut v, RefineBrushMode::Feather);
    v.ps_gesture(Tool::RefineBrush, &[s(16.0, 30.0), s(16.0, 34.0)]).unwrap();
    let f = v.selection().unwrap().clone();
    assert!(f.at(16, 32) < u16::MAX && f.at(15, 32) > 0, "{} {}", f.at(16, 32), f.at(15, 32));
    assert_eq!(f.at(20, 20), u16::MAX);
    assert_eq!(v.selection_steps(), 5);
    // One undo per stroke.
    v.undo_selection().unwrap();
    assert_eq!(v.selection().unwrap().at(15, 32), 0);
    // Changing the selection with another tool restarts the surface (marks dropped).
    v.select_rect(0.0, 0.0, 10.0, 10.0).unwrap();
    mode(&mut v, RefineBrushMode::RefineEdge);
    v.ps_gesture(Tool::RefineBrush, &[s(5.0, 5.0)]).unwrap();
    assert_eq!(v.selection().unwrap().at(50, 50), 0, "the old foreground mark is gone");
}

#[test]
fn refine_brush_tool_and_options_are_drawn() {
    let (_s, mut v) = open_art("refine-ui");
    let ctx = egui::Context::default();
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1800.0, 900.0))), ..Default::default() };
    let _ = ctx.run(input, |ctx| v.ui(ctx));
    let drawn = v.drawn().to_vec();
    assert!(drawn.contains(&widget::tool("refine_edge_brush")), "{drawn:?}");
    for name in [ps::REFINE_MODE, ps::REFINE_SIZE, ps::REFINE_RADIUS] {
        assert!(drawn.iter().any(|d| d == name), "{name}: {drawn:?}");
    }
    assert!(!ps::tool_hover(Tool::RefineBrush).is_empty());
}
