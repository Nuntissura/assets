//! SMB-PS-SELECT-LIQUIFY-2 (feature `viewer`): the magic wand / quick select / magnetic lasso /
//! liquify tool buttons and option row, the canvas actions they call (`ps_click`, `ps_gesture`)
//! and sample-merged sampling of the rendered composite, driven headlessly (no window).
#![cfg(feature = "viewer")]

mod common;

use common::{L, Scratch, layered_psd};
use hsk_studio_harness::view::edit::Tool;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::select_liquify::{self as ps, PsSettings};
use hsk_studio_harness::view::{Viewer, widget};
use hsk_studio_pigment::brush::StrokeSample;
use hsk_studio_pigment::liquify::LiquifyTool;

/// 64x64: "paper" opaque white; "art" above it: a red square x, y 16..40, transparent elsewhere.
fn open_two(label: &str) -> (Scratch, Viewer, String, String) {
    let scratch = Scratch::new(label);
    let path = scratch.0.join("two.psd");
    let n = 64 * 64;
    let inside = |i: usize| (16..40).contains(&(i % 64)) && (16..40).contains(&(i / 64));
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

fn square_errors(v: &Viewer) -> usize {
    let sel = v.selection().expect("a selection");
    let mut wrong = 0;
    for y in 0..64 {
        for x in 0..64 {
            let inside = (16..40).contains(&x) && (16..40).contains(&y);
            wrong += usize::from((sel.at(x, y) >= 32768) != inside);
        }
    }
    wrong
}

#[test]
fn wand_sample_merged_reads_the_composite_and_the_canvas_actions_dispatch() {
    let (_s, mut v, paper, _art) = open_two("ps-merged");
    // The paper layer alone is all white: the wand selects the whole canvas.
    v.select(Some(paper.clone()));
    v.set_tool(Tool::MagicWand);
    v.ps_click(20.5, 20.5).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(v.selection().unwrap().area_sum(), 4096.0);
    v.undo_selection().unwrap();
    // Sample merged: the composite shows the red square over white, so only the square.
    v.set_ps(PsSettings { sample_merged: true, ..PsSettings::default() });
    v.ps_click(20.5, 20.5).unwrap_or_else(|e| panic!("{e}"));
    let wrong = square_errors(&v);
    println!("merged wand: {wrong} pixels differ from the square");
    assert!(wrong <= 4, "{wrong}");
    // Sample merged needs no selected layer.
    v.select(None);
    v.undo_selection().unwrap();
    v.ps_click(20.5, 20.5).unwrap_or_else(|e| panic!("{e}"));
    assert!(square_errors(&v) <= 4);
    // Without sample merged and without a selected layer the action explains itself.
    v.set_ps(PsSettings::default());
    let e = v.ps_click(20.5, 20.5).unwrap_err();
    assert!(e.contains("select a raster layer"), "{e}");
}

#[test]
fn magnetic_lasso_quick_select_and_liquify_gestures() {
    let (_s, mut v, _paper, art) = open_two("ps-gestures");
    let s = |x: f64, y: f64| StrokeSample { x, y, pressure: Some(1.0), time_ms: None };
    // Magnetic lasso: a rough trace 3 px outside the square on the art layer.
    v.select(Some(art.clone()));
    let mut trace = Vec::new();
    for t in (13..43).step_by(2) {
        trace.push(s(f64::from(t), 13.0));
    }
    for t in (13..43).step_by(2) {
        trace.push(s(43.0, f64::from(t)));
    }
    for t in (13..43).rev().step_by(2) {
        trace.push(s(f64::from(t), 43.0));
    }
    for t in (13..43).rev().step_by(2) {
        trace.push(s(13.0, f64::from(t)));
    }
    v.ps_gesture(Tool::MagneticLasso, &trace).unwrap_or_else(|e| panic!("{e}"));
    let wrong = square_errors(&v);
    println!("viewer magnetic lasso: {wrong} pixels differ from the square");
    assert!(wrong <= 160, "{wrong}");
    v.clear_selection();
    // Quick select add on the square.
    v.ps_gesture(Tool::QuickSelect, &[s(26.0, 28.0), s(30.0, 28.0)]).unwrap_or_else(|e| panic!("{e}"));
    let wrong = square_errors(&v);
    println!("viewer quick select gesture: {wrong} pixels differ from the square");
    assert!(wrong <= 8, "{wrong}");
    // Liquify gesture with the twirl tool: one chronicle undo step.
    v.set_ps(PsSettings { liquify_tool: LiquifyTool::Twirl { clockwise: true }, ..PsSettings::default() });
    let before = v.state_json()["edit"]["history"].clone();
    v.ps_gesture(Tool::Liquify, &[s(16.0, 16.0); 6]).unwrap_or_else(|e| panic!("{e}"));
    assert_ne!(v.state_json()["edit"]["history"], before);
    v.undo().unwrap();
}

#[test]
fn one_ui_frame_draws_the_new_tools_and_their_options() {
    let (_s, mut v, _paper, _art) = open_two("ps-ui");
    let ctx = egui::Context::default();
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0))), ..Default::default() };
    let _ = ctx.run(input, |ctx| v.ui(ctx));
    let drawn = v.drawn().to_vec();
    for tool in [Tool::MagicWand, Tool::QuickSelect, Tool::MagneticLasso, Tool::Liquify] {
        assert!(Tool::ALL.contains(&tool));
        assert!(drawn.contains(&widget::tool(tool.key())), "{tool:?} not drawn: {drawn:?}");
        assert!(!ps::tool_hover(tool).is_empty());
    }
    for name in [ps::WAND_TOLERANCE, ps::WAND_CONTIGUOUS, ps::SAMPLE_MERGED, ps::QUICK_SIZE, ps::MAGNETIC_WIDTH, ps::LIQUIFY_TOOL, ps::LIQUIFY_SIZE, ps::LIQUIFY_PRESSURE] {
        assert!(drawn.iter().any(|d| d == name), "{name} not drawn: {drawn:?}");
    }
    assert_eq!(v.state_json()["edit"]["tools"].as_array().unwrap().len(), Tool::ALL.len());
}
