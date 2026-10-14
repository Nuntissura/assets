//! SMB-HARNESS-UI slice 1 (feature `viewer`): the desktop viewer state driven headlessly, no
//! window (CX-SAFE-002). Open a synthetic PSD (independent byte assembly) through the import
//! flow -> composite rendered by render-cpu (pixels checked against the source values) -> hide the
//! top layer through the controls-descriptor `layer.visible` route (chronicle) -> re-render ->
//! save as a native package -> reopen in an independent session and verify. The GPU fallback is
//! proven with render-gpu `BackendChoice::None` (no device is touched); one egui frame runs on a
//! bare `egui::Context` to prove the UI draws its stable widget ids without a window.
#![cfg(feature = "viewer")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::{Viewer, widget};
use hsk_studio_harness::{Harness, References, Selection, default_actor};

mod common;
use common::{Scratch, two_layer_psd};

fn pixel(v: &Viewer, x: usize, y: usize) -> [u8; 4] {
    let f = v.frame().expect("a frame");
    let i = (y * f.width as usize + x) * 4;
    f.rgba8[i..i + 4].try_into().unwrap()
}

fn close(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1)
}

fn layer_id(v: &Viewer, name: &str) -> String {
    v.layers().into_iter().find(|r| r.name == name).unwrap_or_else(|| panic!("layer {name}: {}", v.state_json())).layer_id
}

#[test]
fn viewer_opens_renders_toggles_a_layer_and_saves() {
    let scratch = Scratch::new("toggle");
    let source = scratch.0.join("two.psd");
    std::fs::write(&source, two_layer_psd()).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).expect("opens through the import flow");
    assert_eq!(v.receipt().unwrap()["source_format"], "psd");
    let names: Vec<String> = v.layers().into_iter().map(|r| r.name).collect();
    assert_eq!(names, ["top", "base"], "{}", v.state_json());

    v.render_if_dirty();
    let f = v.frame().unwrap_or_else(|| panic!("{}", v.state_json()));
    assert_eq!((f.width, f.height, f.backend), (2, 2, "cpu"));
    assert!(f.notes.iter().any(|n| n.starts_with("view_root_group")), "{:?}", f.notes);
    // Left column: base only. Right column row 1: the opaque top pixel. Row 0: top at alpha 128
    // over base, composited in linear light (straight alpha out, opaque result).
    assert!(close(pixel(&v, 0, 0), [10, 20, 30, 255]), "{:?}", pixel(&v, 0, 0));
    assert!(close(pixel(&v, 1, 1), [201, 101, 1, 255]), "{:?}", pixel(&v, 1, 1));
    let blended = pixel(&v, 1, 0);
    assert_eq!(blended[3], 255);
    assert!(blended[0] > 100 && blended[0] < 200 && blended[2] < 30, "{blended:?}");
    let first_revision = f.revision;

    let top = layer_id(&v, "top");
    let revision = v.set_visible(&top, false).expect("chronicle commit");
    assert!(revision > first_revision);
    v.render_if_dirty();
    assert_eq!(v.frame().unwrap().revision, revision);
    for (x, y) in [(0, 0), (1, 0), (1, 1)] {
        assert!(close(pixel(&v, x, y), [10, 20, 30, 255]), "({x},{y}) {:?}", pixel(&v, x, y));
    }

    // Save: never over the opened source without overwrite; a new path saves.
    assert!(v.save(&source, false).is_err());
    assert_eq!(std::fs::read(&source).unwrap(), two_layer_psd());
    let out = scratch.0.join("two.handshake");
    v.save(&out, false).expect("saved");
    let ops: Vec<&str> = v.run().ops().iter().map(|o| o["op"].as_str().unwrap()).collect();
    assert_eq!(ops, ["import", "panel_set", "save"], "{}", v.state_json());

    // Independent reopen: the hidden layer stays hidden, the import receipt is carried.
    let h = Harness::new(Selection { providers: Vec::new(), references: References::TrustAll }).unwrap();
    let cancel = CancellationToken::default();
    let mut run = h.run(default_actor(), &cancel);
    assert!(run.open_path(&out));
    assert!(run.verify_live(Some(revision), vec![(top.clone(), "visible", serde_json::json!(false))]));
    assert_eq!(run.finish("verify")["verdict"], "pass");
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&out).expect("the saved package opens in the viewer");
    assert_eq!(again.receipt().unwrap()["source_format"], "psd");
    again.render_if_dirty();
    assert!(close(pixel(&again, 1, 1), [10, 20, 30, 255]));
}

#[test]
fn gpu_unavailable_falls_back_to_render_cpu() {
    let scratch = Scratch::new("fallback");
    let source = scratch.0.join("two.psd");
    std::fs::write(&source, two_layer_psd()).unwrap();
    let options = hsk_studio_render_gpu::DeviceOptions { backend: hsk_studio_render_gpu::BackendChoice::None, ..Default::default() };
    let mut v = Viewer::with_gpu_options(Prefer::Gpu, options);
    v.open(&source).unwrap();
    v.render_if_dirty();
    let f = v.frame().unwrap_or_else(|| panic!("{}", v.state_json()));
    assert_eq!(f.backend, "cpu");
    assert!(f.notes.iter().any(|n| n.starts_with("gpu_fallback")), "{:?}", f.notes);
    assert!(close(pixel(&v, 1, 1), [201, 101, 1, 255]));
}

#[test]
fn one_ui_frame_draws_the_stable_widget_ids_without_a_window() {
    let scratch = Scratch::new("ui");
    let source = scratch.0.join("two.psd");
    std::fs::write(&source, two_layer_psd()).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    let top = layer_id(&v, "top");
    v.select(Some(top.clone()));
    let ctx = egui::Context::default();
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0))), ..Default::default() };
    let _ = ctx.run(input, |ctx| v.ui(ctx));
    let drawn = v.drawn().to_vec();
    for name in [widget::PATH_FIELD, widget::OPEN_BUTTON, widget::SAVE_BUTTON, widget::LAYER_PANEL, widget::PROPERTIES_PANEL, widget::RECEIPT_PANEL, widget::CANVAS] {
        assert!(drawn.iter().any(|d| d == name), "{name} not drawn: {drawn:?}");
    }
    assert!(drawn.contains(&widget::layer(&top, "visible")), "{drawn:?}");
    // The frame rendered during the UI pass (the document was dirty after open).
    assert_eq!(v.frame().map(|f| f.backend), Some("cpu"), "{}", v.state_json());
    // Nothing was committed by drawing.
    assert_eq!(v.run().ops().len(), 1);
}
