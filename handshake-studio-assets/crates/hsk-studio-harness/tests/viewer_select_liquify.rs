//! SMB-PS-SELECT-LIQUIFY (feature `viewer`): magic wand, quick selection and the liquify brush in
//! the viewer, driven headlessly through the `Viewer` methods (no window, CX-SAFE-002): each wand /
//! quick-select call is one selection-history step, each liquify stroke one chronicle undo step.
#![cfg(feature = "viewer")]

mod common;

use common::{L, Scratch, layered_psd};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_pigment::brush::StrokeSample;
use hsk_studio_pigment::liquify::{LiquifyBrush, LiquifyTool};
use hsk_studio_pigment::select_tools::quick::StrokeMode;
use hsk_studio_pigment::select_tools::wand::WandOptions;

/// 64x64 opaque layer "art": white with a red square x 16..40, y 16..40.
fn open_art(label: &str) -> (Scratch, Viewer, String) {
    let scratch = Scratch::new(label);
    let path = scratch.0.join("art.psd");
    let n = 64 * 64;
    let red = |x: usize, y: usize| (16..40).contains(&x) && (16..40).contains(&y);
    let ch = |c: u8| -> Vec<u8> { (0..n).map(|i| if red(i % 64, i / 64) { if c == 0 { 255 } else { 0 } } else { 255 }).collect() };
    let psd = layered_psd(64, 64, &[L { name: "art", rect: [0, 0, 64, 64], channels: vec![(0, ch(0)), (1, ch(1)), (2, ch(2))] }]);
    std::fs::write(&path, psd).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&path).unwrap();
    let id = v.layers().into_iter().find(|r| r.name == "art").unwrap_or_else(|| panic!("{}", v.state_json())).layer_id;
    (scratch, v, id)
}

fn pixel(v: &mut Viewer, x: usize, y: usize) -> [u8; 4] {
    v.render_if_dirty();
    let f = v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json()));
    let i = (y * f.width as usize + x) * 4;
    f.rgba8[i..i + 4].try_into().unwrap()
}

fn s(x: f64, y: f64) -> StrokeSample {
    StrokeSample { x, y, pressure: Some(1.0), time_ms: None }
}

#[test]
fn wand_and_quick_select_are_selection_steps() {
    let (_scratch, mut v, art) = open_art("wand-quick");
    v.select_wand(&art, 20, 20, &WandOptions::default()).unwrap_or_else(|e| panic!("{e}"));
    let sel = v.selection().expect("wand selection").clone();
    assert_eq!(sel.at(20, 20), u16::MAX);
    assert_eq!(sel.at(38, 38), u16::MAX);
    // Anti-alias (document toggle on): the inner corner pixel holds 4 of 9 selected neighbours.
    assert_eq!(sel.at(39, 39), ((4u64 * 65535 * 2 + 9) / 18) as u16);
    assert_eq!(sel.bounds, hsk_studio_pigment::Rect { x: 15, y: 15, width: 26, height: 26 });
    assert_eq!(sel.at(45, 20), 0);
    assert!(sel.at(16, 20) >= 32768 && sel.at(15, 20) < 32768, "edge {} {}", sel.at(16, 20), sel.at(15, 20));
    assert!(v.ant_segments() > 0);
    assert_eq!(v.selection_steps(), 1);
    v.undo_selection().unwrap();
    assert!(v.selection().is_none());
    assert!(v.undo_selection().is_err());
    // Marquee (not a history step) around the square plus white margin, then a quick-select
    // subtract stroke on the top white margin: inside the stroke's ROI (footprint y 8..12 grown by
    // 3 * 4 + 8 = 20 -> rows 0..32) the white goes and the red square stays; below the ROI the
    // marquee is unchanged (quick select is local to its ROI).
    v.select_rect(8.0, 8.0, 40.0, 40.0).unwrap();
    let rect = v.selection().unwrap().clone();
    v.select_quick(&art, &[s(10.0, 10.0), s(46.0, 10.0)], 4.0, StrokeMode::Subtract).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(v.selection_steps(), 1);
    let kept = v.selection().expect("square kept").clone();
    let mut wrong = 0;
    for y in 0..64 {
        for x in 0..64 {
            let square = (16..40).contains(&x) && (16..40).contains(&y);
            let inside = if y < 32 { square } else { (8..48).contains(&x) && (8..48).contains(&y) };
            wrong += usize::from((kept.at(x, y) >= 32768) != inside);
        }
    }
    println!("viewer quick subtract: {wrong} pixels differ from the expected selection");
    assert!(wrong <= 8, "{wrong}");
    v.undo_selection().unwrap();
    assert_eq!(v.selection().unwrap(), &rect);
    v.clear_selection();
    // Quick select add on the square from nothing finds the square.
    v.select_quick(&art, &[s(26.0, 28.0), s(30.0, 28.0)], 6.0, StrokeMode::Add).unwrap_or_else(|e| panic!("{e}"));
    let q = v.selection().expect("quick selection");
    let mut wrong = 0;
    for y in 0..64 {
        for x in 0..64 {
            let inside = (16..40).contains(&x) && (16..40).contains(&y);
            wrong += usize::from((q.at(x, y) >= 32768) != inside);
        }
    }
    println!("viewer quick select: {wrong} pixels differ from the square");
    assert!(wrong <= 8, "{wrong}");
}

#[test]
fn liquify_stroke_is_one_chronicle_undo_step() {
    let (_scratch, mut v, art) = open_art("liquify");
    let before = pixel(&mut v, 44, 28);
    assert_eq!(before, [255, 255, 255, 255]);
    // Forward-warp the square's right edge 8 px to the right at rows ~28.
    let brush = LiquifyBrush { size: 24.0, density: 100.0, pressure: 100.0, rate: 80.0 };
    let r1 = v.liquify(&art, LiquifyTool::ForwardWarp, &[s(38.0, 28.0), s(45.2, 28.0)], &brush).unwrap_or_else(|e| panic!("{e}"));
    let warped = pixel(&mut v, 44, 28);
    assert!(warped[1] < 64 && warped[0] > 200, "red pushed into x = 44: {warped:?}");
    assert_eq!(pixel(&mut v, 5, 5), [255, 255, 255, 255], "outside the brush");
    let history = v.state_json()["edit"]["history"].clone();
    // One step: undo restores the exact pixels, redo re-applies.
    let r0 = v.undo().unwrap();
    assert_ne!(r0, r1);
    assert_eq!(pixel(&mut v, 44, 28), before);
    v.redo().unwrap();
    assert_eq!(pixel(&mut v, 44, 28), warped, "{history}");
}

#[test]
fn tool_refusals_are_typed() {
    let (_scratch, mut v, art) = open_art("refuse");
    let e = v.select_wand("nope", 1, 1, &WandOptions::default()).unwrap_err();
    assert!(e.contains("unknown layer"), "{e}");
    let e = v.select_wand(&art, -5, 1, &WandOptions::default()).unwrap_err();
    assert!(e.contains("invalid_input"), "{e}");
    // Sample merged reads the rendered composite (lane SMB-PS-SELECT-LIQUIFY-2): one step, undone.
    v.select_wand(&art, 1, 1, &WandOptions { sample_merged: true, ..WandOptions::default() }).unwrap_or_else(|e| panic!("{e}"));
    v.undo_selection().unwrap();
    let e = v.select_quick(&art, &[s(f64::NAN, 1.0)], 4.0, StrokeMode::Add).unwrap_err();
    assert!(e.contains("nonfinite"), "{e}");
    let e = v.liquify(&art, LiquifyTool::Twirl { clockwise: true }, &[s(1.0, 1.0)], &LiquifyBrush { size: 1.0e9, ..LiquifyBrush::default() }).unwrap_err();
    assert!(e.contains("brush_out_of_range"), "{e}");
    let e = v.liquify(&art, LiquifyTool::ForwardWarp, &[s(1.0, 1.0)], &LiquifyBrush::default()).unwrap_err();
    assert!(e.contains("moved no pixel"), "{e}");
    assert_eq!(v.selection_steps(), 0);
}
