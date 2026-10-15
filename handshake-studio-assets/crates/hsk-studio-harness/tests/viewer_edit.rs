//! SMB-HARNESS-UI-2 (feature `viewer`): editing in the viewer, driven headlessly through the same
//! `Viewer` methods the egui tools call (no window, CX-SAFE-002): selections with combine modes,
//! brush painting clipped to the selection, undo/redo, save/reopen with pixel checks; opacity and
//! blend-mode edits through chronicle with undo/redo; one UI frame draws the tool ids (and, with
//! feature `desktop`, produces an AccessKit tree).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd, two_layer_psd};
use hsk_studio_harness::view::edit::{BrushSettings, Tool};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::{Viewer, widget};
use hsk_studio_pigment::brush::StrokeSample;
use hsk_studio_pigment::selection::CombineMode;

fn pixel(v: &Viewer, x: usize, y: usize) -> [u8; 4] {
    let f = v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json()));
    let i = (y * f.width as usize + x) * 4;
    f.rgba8[i..i + 4].try_into().unwrap()
}

fn close(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1)
}

fn layer_id(v: &Viewer, name: &str) -> String {
    v.layers().into_iter().find(|r| r.name == name).unwrap_or_else(|| panic!("layer {name}: {}", v.state_json())).layer_id
}

fn line(y: f64, x0: f64, x1: f64) -> Vec<StrokeSample> {
    [x0, x1].iter().map(|&x| StrokeSample { x, y, pressure: Some(1.0), time_ms: None }).collect()
}

const WHITE: [u8; 4] = [255, 255, 255, 255];
const RED: [u8; 4] = [255, 0, 0, 255];

#[test]
fn select_paint_undo_redo_save_reopen_pixels() {
    let scratch = Scratch::new("edit-paint");
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(32, 32, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    // The convert flow states the blending space: the viewer no longer assumes it.
    let state = v.state_json();
    assert_eq!(state["receipt"]["blending_space"]["bound_to"], "working_space_rgb", "{state}");
    v.render_if_dirty();
    assert!(!v.frame().unwrap().notes.iter().any(|n| n.starts_with("view_blending_space_assumed")), "{:?}", v.frame().unwrap().notes);
    assert!(close(pixel(&v, 4, 16), WHITE));
    let paper = layer_id(&v, "paper");

    // Selection: left half (replace) + ellipse on the right (extend) - lasso triangle (diminish).
    v.select_rect(0.0, 0.0, 16.0, 32.0).unwrap();
    v.set_combine(CombineMode::Extend);
    v.select_ellipse(24.0, 24.0, 8.0, 8.0).unwrap();
    v.set_combine(CombineMode::Diminish);
    v.select_lasso(&[[0.0, 0.0], [8.0, 0.0], [0.0, 8.0]]).unwrap();
    let sel = v.selection().expect("a selection");
    assert_eq!(sel.at(10, 16), u16::MAX, "inside the rectangle");
    assert_eq!(sel.at(28, 28), u16::MAX, "inside the extended ellipse");
    assert_eq!(sel.at(20, 16), 0, "outside both");
    assert_eq!(sel.at(1, 1), 0, "diminished by the lasso");
    assert!(v.ant_segments() > 0, "marching ants outline");
    assert_eq!(v.state_json()["edit"]["selection"]["bounds"][2], 32);

    // Paint a hard red line across the whole width at y = 16: only the selected half changes.
    v.set_tool(Tool::Brush);
    v.set_brush(BrushSettings { diameter_px: 8.0, hardness_pct: 100.0, spacing_pct: 10.0, opacity_pct: 100.0, rgb8: [255, 0, 0] });
    let painted = v.paint(&paper, &line(16.0, 0.0, 32.0)).unwrap_or_else(|e| panic!("{e}"));
    v.render_if_dirty();
    assert_eq!(v.frame().unwrap().revision, painted);
    assert!(close(pixel(&v, 4, 16), RED), "{:?}", pixel(&v, 4, 16));
    assert!(close(pixel(&v, 24, 16), WHITE), "clipped by the selection: {:?}", pixel(&v, 24, 16));
    assert!(close(pixel(&v, 4, 4), WHITE), "outside the stroke");

    // Save the painted state, reopen independently: the staged tile was written.
    let painted_path = scratch.0.join("painted.handshake");
    v.save(&painted_path, false).unwrap();
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&painted_path).unwrap();
    again.render_if_dirty();
    assert!(close(pixel(&again, 4, 16), RED) && close(pixel(&again, 24, 16), WHITE));

    // Undo -> white, redo -> red, undo -> white (history is chronicle's inverse chain).
    assert_eq!(v.state_json()["edit"]["history"]["undo"].as_array().unwrap().len(), 1);
    v.undo().unwrap();
    v.render_if_dirty();
    assert!(close(pixel(&v, 4, 16), WHITE), "undo: {:?}", pixel(&v, 4, 16));
    v.redo().unwrap();
    v.render_if_dirty();
    assert!(close(pixel(&v, 4, 16), RED), "redo");
    v.undo().unwrap();
    v.render_if_dirty();
    assert!(close(pixel(&v, 4, 16), WHITE));
    assert!(v.undo().is_err(), "nothing left to undo");

    // Save after undo (the painted state was saved before: the original tile is retained),
    // reopen: the original pixels.
    let undone = scratch.0.join("undone.handshake");
    v.save(&undone, false).unwrap();
    let mut back = Viewer::new(Prefer::Cpu);
    back.open(&undone).unwrap();
    back.render_if_dirty();
    for (x, y) in [(4, 16), (24, 16), (4, 4)] {
        assert!(close(pixel(&back, x, y), WHITE), "({x},{y}) {:?}", pixel(&back, x, y));
    }
    // Every action is recorded in the run log.
    let ops: Vec<&str> = v.run().ops().iter().map(|o| o["op"].as_str().unwrap()).collect();
    assert_eq!(ops, ["import", "paint", "save", "undo", "redo", "undo", "undo", "save"], "{}", v.state_json());
}

#[test]
fn opacity_and_blend_mode_edits_with_undo_redo_survive_save() {
    let scratch = Scratch::new("edit-appearance");
    let source = scratch.0.join("two.psd");
    std::fs::write(&source, two_layer_psd()).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.render_if_dirty();
    let opaque = pixel(&v, 1, 1);
    assert!(close(opaque, [201, 101, 1, 255]));
    let top = layer_id(&v, "top");

    v.set_opacity(&top, 50.0).unwrap();
    v.render_if_dirty();
    let half = pixel(&v, 1, 1);
    assert!(half[0] < 190 && half[0] > 100 && half[1] > 40, "50 % top over base: {half:?}");
    v.undo().unwrap();
    v.render_if_dirty();
    assert!(close(pixel(&v, 1, 1), opaque), "undo restores the pixels");
    assert!(v.layers().iter().any(|r| r.layer_id == top && r.opacity == 100.0));
    v.redo().unwrap();
    v.render_if_dirty();
    assert_eq!(pixel(&v, 1, 1), half, "redo re-applies");

    // Multiply (discriminant 5) darkens; the controls panel route commits too (opacity 25).
    v.set_blend_mode(&top, 5).unwrap();
    v.render_if_dirty();
    let multiplied = pixel(&v, 1, 1);
    assert!(multiplied[0] < half[0], "{multiplied:?} vs {half:?}");
    assert!(v.run().history()["undo"].as_array().unwrap().len() == 2);
    v.undo().unwrap();
    v.render_if_dirty();
    assert_eq!(pixel(&v, 1, 1), half);
    assert!(v.set_blend_mode(&top, 24).is_err(), "tool-only behind is refused by folio");

    let out = scratch.0.join("appearance.handshake");
    v.save(&out, false).unwrap();
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&out).unwrap();
    assert!(again.layers().iter().any(|r| r.layer_id == top && r.opacity == 50.0), "{}", again.state_json());
    again.render_if_dirty();
    assert_eq!(pixel(&again, 1, 1), half);
    // The text tool is available (SMB-HARNESS-UI-3 slice 4; proofs in tests/viewer_text.rs).
    assert_eq!(again.state_json()["edit"]["text_tool"]["available"], true);
}

#[test]
fn one_ui_frame_draws_the_tool_ids() {
    let scratch = Scratch::new("edit-ui");
    let source = scratch.0.join("two.psd");
    std::fs::write(&source, two_layer_psd()).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.select_rect(0.0, 0.0, 1.0, 2.0).unwrap();
    let ctx = egui::Context::default();
    #[cfg(feature = "desktop")]
    ctx.enable_accesskit();
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))), ..Default::default() };
    let output = ctx.run(input, |ctx| v.ui(ctx));
    let drawn = v.drawn().to_vec();
    for tool in Tool::ALL {
        assert!(drawn.contains(&widget::tool(tool.key())), "{tool:?} not drawn: {drawn:?}");
    }
    for name in [widget::UNDO_BUTTON, widget::REDO_BUTTON, widget::COMBINE, widget::CLEAR_SELECTION, widget::BRUSH_SIZE, widget::BRUSH_COLOUR] {
        assert!(drawn.iter().any(|d| d == name), "{name} not drawn: {drawn:?}");
    }
    // Drawing committed nothing and kept the selection.
    assert_eq!(v.run().ops().len(), 1);
    assert!(v.selection().is_some());
    #[cfg(feature = "desktop")]
    {
        let tree = output.platform_output.accesskit_update.expect("AccessKit tree with feature desktop");
        assert!(tree.nodes.len() > 20, "AccessKit nodes: {}", tree.nodes.len());
    }
    #[cfg(not(feature = "desktop"))]
    let _ = output;
}
