//! Lane SMB-PS-INFO-HISTOGRAM-1 (feature `viewer`): the Info panel in the window (STU-RAS-049,
//! STU-RAS-163). Headless egui; the layout is settled before any pointer event (F-TL-2). Oracles:
//! Pigment's own `diagnostics` results over the rendered frame bytes (`Viewer::frame().rgba8`), the
//! session's undo history and document entry, and `state_json()["info"]`.
//! NOT_PROVEN: equality with Photoshop's Histogram / Info panels and colour samplers (no D5 case for
//! PS-30); the GPU shared-device display path and a real desktop window.
#![cfg(feature = "viewer")]

mod support_context;

#[path = "../../hsk-studio-harness/tests/common/mod.rs"]
mod common;

use common::{L, Scratch, layered_psd};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::info::{self, InfoCommand, InfoRecords, Pick, SampleScope};
use hsk_studio_harness_view::view::render::Prefer;
use hsk_studio_harness_view::view::shell::{Panel, ShellCommand};
use hsk_studio_pigment::diagnostics::{self as diag, ColourModel, HistogramChannel, Rgba8Plane, SampleSize};
use serde_json::{Value, json};
use std::path::PathBuf;

const W: u32 = 64;
const H: u32 = 48;
const SCREEN: (f32, f32) = (1400.0, 900.0);
const RED: [u8; 3] = [200, 30, 40];
const GREEN: [u8; 3] = [20, 180, 60];

fn ctrl_shift() -> egui::Modifiers {
    egui::Modifiers { ctrl: true, shift: true, command: true, ..Default::default() }
}

fn frame(v: &mut Viewer, ctx: &egui::Context, events: Vec<egui::Event>, modifiers: egui::Modifiers) {
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN.0, SCREEN.1))), events, modifiers, ..Default::default() };
    let _ = ctx.run(input, |ctx| v.ui(ctx));
}

fn settle(v: &mut Viewer, ctx: &egui::Context) {
    for _ in 0..4 {
        frame(v, ctx, vec![], egui::Modifiers::NONE);
    }
}

fn info(v: &Viewer) -> Value {
    v.state_json()["info"].clone()
}

fn drew(v: &Viewer, name: &str) -> bool {
    v.drawn().iter().any(|d| d == name)
}

/// Bottom layer `red` fills the canvas; top layer `green` covers the right half (x >= 32).
fn two_layers(scratch: &Scratch) -> PathBuf {
    let n = (W * H) as usize;
    let half = (W / 2 * H) as usize;
    let bytes = layered_psd(
        W,
        H,
        &[
            L { name: "red", rect: [0, 0, H as i32, W as i32], channels: vec![(0, vec![RED[0]; n]), (1, vec![RED[1]; n]), (2, vec![RED[2]; n])] },
            L { name: "green", rect: [0, (W / 2) as i32, H as i32, W as i32], channels: vec![(0, vec![GREEN[0]; half]), (1, vec![GREEN[1]; half]), (2, vec![GREEN[2]; half])] },
        ],
    );
    let path = scratch.0.join("two.psd");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn opened(label: &str) -> (Scratch, Viewer) {
    let scratch = Scratch::new(&format!("info-{label}"));
    let path = two_layers(&scratch);
    let mut v = crate::support_context::viewer(Prefer::Cpu);
    v.open(&path).unwrap_or_else(|e| panic!("open: {e}"));
    v.render_if_dirty();
    (scratch, v)
}

fn layer_id(v: &Viewer, name: &str) -> String {
    v.layers().into_iter().find(|r| r.name == name).unwrap_or_else(|| panic!("layer {name}")).layer_id
}

/// The rendered frame as a pigment plane, and Pigment's own sample of a document pixel.
fn frame_sample(v: &Viewer, x: i64, y: i64, size: SampleSize) -> Option<diag::Sample> {
    let f = v.frame().expect("frame");
    let p = Rgba8Plane { x: f.x, y: f.y, width: f.width, height: f.height, data: &f.rgba8 };
    diag::sample(&p, x, y, size).unwrap()
}

fn frame_histogram(v: &Viewer) -> diag::Histogram {
    let f = v.frame().expect("frame");
    let p = Rgba8Plane { x: f.x, y: f.y, width: f.width, height: f.height, data: &f.rgba8 };
    diag::histogram(&p, None, 256).unwrap()
}

/// The screen point of document pixel centre `(x, y)` from the panel's canvas mapping.
fn screen_of(v: &Viewer, x: f64, y: f64) -> egui::Pos2 {
    let view = info(v)["view"].clone();
    assert!(view.is_object(), "no canvas mapping: {}", info(v));
    let f = |k: &str, i: usize| view[k][i].as_f64().unwrap();
    let zoom = view["zoom"].as_f64().unwrap();
    egui::pos2((f("shown_min", 0) + (x + 0.5 - f("origin", 0)) * zoom) as f32, (f("shown_min", 1) + (y + 0.5 - f("origin", 1)) * zoom) as f32)
}

fn dominant_green(rgba: &Value) -> bool {
    let c: Vec<u64> = rgba.as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    c[1] > c[0] && c[1] > c[2] && c[3] == 255
}

fn dominant_red(rgba: &Value) -> bool {
    let c: Vec<u64> = rgba.as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    c[0] > c[1] && c[0] > c[2] && c[3] == 255
}

fn open_info(v: &mut Viewer, ctx: &egui::Context) {
    v.shell_command(ShellCommand::Panel(Panel::Info)).unwrap();
    settle(v, ctx);
    assert!(drew(v, info::widget::PANEL), "{:?}", v.drawn());
}

#[test]
fn info_opens_from_the_workspace_menu_and_shortcut() {
    println!("NOT_PROVEN: the Info panel's GPU shared-device display path (the panel reads the render-cpu read-back) and a real desktop window");
    let mut v = crate::support_context::viewer(Prefer::Cpu);
    let ctx = egui::Context::default();
    settle(&mut v, &ctx);
    let panel = |v: &Viewer| v.state_json()["shell"]["panels"]["info"].clone();
    // Closed by default; the WORKSPACE > Panels entry is available with no document (always relevant).
    assert_eq!((panel(&v)["open"].as_bool(), panel(&v)["availability"].as_str()), (Some(false), Some("available")));
    assert_eq!((panel(&v)["label"].as_str(), panel(&v)["shortcut"].as_str(), panel(&v)["command"].as_str()), (Some("Info"), Some("Ctrl+Shift+F1"), Some("studio.workspace.panel.info")));
    let bar = v.menu_bar();
    let node = bar.find("studio.workspace.panel.info").expect("WORKSPACE > Panels > Info entry");
    assert!(node.availability.is_available());
    assert!(!drew(&v, info::widget::PANEL));
    // Ctrl+Shift+F1 opens it; the plain F1 does nothing.
    frame(&mut v, &ctx, vec![egui::Event::Key { key: egui::Key::F1, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }], egui::Modifiers::NONE);
    assert_eq!(panel(&v)["open"], false);
    frame(&mut v, &ctx, vec![egui::Event::Key { key: egui::Key::F1, physical_key: None, pressed: true, repeat: false, modifiers: ctrl_shift() }], ctrl_shift());
    frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    assert_eq!((panel(&v)["open"].as_bool(), panel(&v)["visible"].as_bool()), (Some(true), Some(true)), "{}", v.state_json()["status"]);
    assert!(drew(&v, info::widget::PANEL) && drew(&v, info::widget::HISTOGRAM) && drew(&v, info::widget::MODEL), "{:?}", v.drawn());
    assert_eq!(info(&v)["visible"], true);
    assert!(info(&v)["document"].is_null() && info(&v)["histogram"].is_null());
    // The menu entry (the same command id the menu click resolves) hides it, then shows it again.
    let cmd = ShellCommand::from_id("studio.workspace.panel.info").expect("menu command");
    v.shell_command(cmd).unwrap();
    frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    assert!(!drew(&v, info::widget::PANEL));
    assert_eq!((panel(&v)["open"].as_bool(), panel(&v)["visible"].as_bool()), (Some(true), Some(false)));
    v.shell_command(cmd).unwrap();
    frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    assert!(drew(&v, info::widget::PANEL));
    // Drawing committed nothing.
    assert!(v.run().ops().is_empty());
}

#[test]
fn hovering_the_canvas_reads_the_pixel_under_the_pointer() {
    println!("NOT_PROVEN: equality with Photoshop's Info panel readout (no D5 case for PS-30)");
    let (_s, mut v) = opened("hover");
    let ctx = egui::Context::default();
    open_info(&mut v, &ctx);
    let i = info(&v);
    assert_eq!((i["document"]["width"].as_u64(), i["document"]["height"].as_u64(), i["document"]["unit"].as_str()), (Some(u64::from(W)), Some(u64::from(H)), Some("pixels")));
    assert!(i["pointer"].is_null(), "{}", i["pointer"]);
    for (x, y, green) in [(10i64, 10i64, false), (50, 30, true), (31, 0, false), (32, 47, true)] {
        let p = screen_of(&v, x as f64, y as f64);
        frame(&mut v, &ctx, vec![egui::Event::PointerMoved(p)], egui::Modifiers::NONE);
        frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
        let ptr = info(&v)["pointer"].clone();
        assert_eq!(ptr["pixel"], json!([x, y]), "{ptr}");
        assert_eq!(ptr["in_bounds"], true);
        let want = frame_sample(&v, x, y, SampleSize::Point).unwrap();
        assert_eq!(ptr["rgba8"], json!(want.rgba8), "({x}, {y})");
        assert!(if green { dominant_green(&ptr["rgba8"]) } else { dominant_red(&ptr["rgba8"]) }, "({x}, {y}): {ptr}");
        assert_eq!(ptr["readout"]["values"], json!(diag::readout(want.rgb, ColourModel::Rgb).unwrap()));
    }
    // The readout model and the sample size are document state; the pointer readout follows them.
    v.info_command(InfoCommand::SetModel(ColourModel::Lab)).unwrap();
    v.info_command(InfoCommand::SetSampleSize(SampleSize::S5)).unwrap();
    frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    let ptr = info(&v)["pointer"].clone();
    let want = frame_sample(&v, 32, 47, SampleSize::S5).unwrap();
    assert_eq!((ptr["pixels"].as_u64(), ptr["readout"]["model"].as_str()), (Some(u64::from(want.pixels)), Some("lab")));
    assert_eq!(ptr["readout"]["values"], json!(diag::readout(want.rgb, ColourModel::Lab).unwrap()));
    assert_eq!(ptr["readout"]["components"], json!(["L", "a", "b"]));
    // Off the canvas: no pointer readout.
    frame(&mut v, &ctx, vec![egui::Event::PointerMoved(egui::pos2(5.0, SCREEN.1 - 5.0))], egui::Modifiers::NONE);
    frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    assert!(info(&v)["pointer"].is_null() || info(&v)["pointer"]["in_bounds"] == false, "{}", info(&v)["pointer"]);
}

#[test]
fn histogram_numbers_are_in_state_json() {
    println!("NOT_PROVEN: equality with Photoshop's Histogram panel (no D5 case for PS-30)");
    let (_s, mut v) = opened("histogram");
    let ctx = egui::Context::default();
    open_info(&mut v, &ctx);
    let h = info(&v)["histogram"].clone();
    let want = frame_histogram(&v);
    assert_eq!((h["bins"].as_u64(), h["pixels"].as_u64(), h["transparent"].as_u64()), (Some(256), Some(u64::from(W * H)), Some(0)), "{h}");
    assert_eq!(info(&v)["region"], "document");
    for c in HistogramChannel::ALL {
        let got = &h["channels"][c.code()];
        let w = want.channel(c).unwrap();
        assert_eq!(got["bins"], json!(w.bins), "{c:?}");
        assert_eq!((got["count"].as_u64(), got["clip_low"].as_u64(), got["clip_high"].as_u64(), got["median"].as_u64()), (Some(w.count), Some(w.clip_low), Some(w.clip_high), Some(u64::from(w.median))), "{c:?}");
        assert_eq!(got["mean"].as_f64(), Some(w.mean));
    }
    // Two colours, half the canvas each: the red channel has exactly two filled bins of W*H/2.
    let red: Vec<u64> = h["channels"]["red"]["bins"].as_array().unwrap().iter().map(|b| b.as_u64().unwrap()).collect();
    let filled: Vec<u64> = red.iter().copied().filter(|n| *n > 0).collect();
    assert_eq!(filled, vec![u64::from(W * H / 2); 2], "{red:?}");
    assert_eq!(h["channels"]["alpha"]["clip_high"].as_u64(), Some(u64::from(W * H)));
    assert_eq!(h["luminosity_weights_per_mille"], json!([299, 587, 114]));
    // A selection: the histogram covers it (rect over the left, red half only) and names its bounds.
    v.select_rect(4.0, 6.0, 10.0, 8.0).unwrap();
    frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    let i = info(&v);
    assert_eq!(i["region"], "selection");
    assert_eq!(i["selection"]["bounds"], json!({"x": 4, "y": 6, "width": 10, "height": 8}), "{}", i["selection"]);
    assert_eq!(i["selection"]["pixels"].as_u64(), Some(80));
    assert_eq!(i["histogram"]["pixels"].as_u64(), Some(80));
    let red: Vec<u64> = i["histogram"]["channels"]["red"]["bins"].as_array().unwrap().iter().map(|b| b.as_u64().unwrap()).collect();
    assert_eq!(red.iter().filter(|n| **n > 0).count(), 1);
    // Clip counts: a canvas with no 0 / 255 colour samples has none in the colour channels.
    assert_eq!(i["histogram"]["channels"]["luminosity"]["clip_low"].as_u64(), Some(0));
}

#[test]
fn histogram_updates_after_an_edit() {
    let (_s, mut v) = opened("edit");
    let ctx = egui::Context::default();
    open_info(&mut v, &ctx);
    let before = info(&v)["histogram"].clone();
    let rev = v.state_json()["revision"].clone();
    let green = layer_id(&v, "green");
    v.set_visible(&green, false).unwrap();
    frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    assert_ne!(v.state_json()["revision"], rev);
    let after = info(&v)["histogram"].clone();
    assert_ne!(before["channels"]["red"]["bins"], after["channels"]["red"]["bins"]);
    // Now every pixel is the red layer: one filled bin of W*H per colour channel, equal to Pigment's.
    let want = frame_histogram(&v);
    assert_eq!(after["channels"]["green"]["bins"], json!(want.channel(HistogramChannel::Green).unwrap().bins));
    let red: Vec<u64> = after["channels"]["red"]["bins"].as_array().unwrap().iter().map(|b| b.as_u64().unwrap()).collect();
    assert_eq!(red.iter().copied().filter(|n| *n > 0).collect::<Vec<_>>(), vec![u64::from(W * H)]);
    // Undo brings the first histogram back.
    v.undo().unwrap();
    frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    assert_eq!(info(&v)["histogram"]["channels"], before["channels"]);
}

#[test]
fn four_samplers_read_live_values() {
    println!("NOT_PROVEN: equality with Photoshop's colour samplers (no D5 case for PS-30)");
    let (_s, mut v) = opened("samplers");
    let ctx = egui::Context::default();
    open_info(&mut v, &ctx);
    let points = [(5.5, 5.5), (40.25, 10.0), (63.0, 47.0), (31.9, 20.0)];
    for (x, y) in points {
        v.info_command(InfoCommand::AddSampler { x, y }).unwrap();
    }
    v.info_command(InfoCommand::SetSamplerModel { index: 1, model: ColourModel::Hsb }).unwrap();
    v.info_command(InfoCommand::SetSamplerModel { index: 2, model: ColourModel::Cmyk }).unwrap();
    frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    let samplers = info(&v)["samplers"].clone();
    assert_eq!(samplers.as_array().unwrap().len(), 4);
    let models = [ColourModel::Rgb, ColourModel::Hsb, ColourModel::Cmyk, ColourModel::Rgb];
    for (i, ((x, y), m)) in points.iter().zip(models).enumerate() {
        let s = &samplers[i];
        let (px, py) = (x.floor() as i64, y.floor() as i64);
        let want = frame_sample(&v, px, py, SampleSize::Point).unwrap();
        assert_eq!((s["pixel"].clone(), s["rgba8"].clone()), (json!([px, py]), json!(want.rgba8)), "sampler {i}");
        assert_eq!(s["readout"]["model"], m.code());
        assert_eq!(s["readout"]["values"], json!(diag::readout(want.rgb, m).unwrap()), "sampler {i}");
        assert!(drew(&v, &info::widget::sampler_remove(i)));
    }
    assert!(dominant_green(&samplers[1]["rgba8"]) && dominant_red(&samplers[0]["rgba8"]));
    // Live: hiding the green layer changes sampler 1 to red with no sampler command.
    let green = layer_id(&v, "green");
    v.set_visible(&green, false).unwrap();
    frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    assert!(dominant_red(&info(&v)["samplers"][1]["rgba8"]), "{}", info(&v)["samplers"][1]);
    v.set_visible(&green, true).unwrap();
    // Each command is one undo step; move and remove.
    let undo = v.run().history()["undo"].as_array().unwrap().len();
    v.info_command(InfoCommand::MoveSampler { index: 0, x: 50.0, y: 1.0 }).unwrap();
    assert_eq!(v.run().history()["undo"].as_array().unwrap().len(), undo + 1);
    frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    assert!(dominant_green(&info(&v)["samplers"][0]["rgba8"]));
    v.undo().unwrap();
    assert_eq!(info(&v)["samplers"][0]["x"].as_f64(), Some(5.5));
    // Up to MAX_SAMPLERS; one more is refused and changes nothing.
    for k in 4..info::MAX_SAMPLERS {
        v.info_command(InfoCommand::AddSampler { x: k as f64, y: 2.0 }).unwrap();
    }
    let entry = v.info_records().unwrap();
    assert!(v.info_command(InfoCommand::AddSampler { x: 1.0, y: 1.0 }).unwrap_err().contains("info_too_many_samplers"));
    assert!(v.info_command(InfoCommand::RemoveSampler(99)).unwrap_err().contains("no sampler 99"));
    assert!(v.info_command(InfoCommand::AddSampler { x: f64::NAN, y: 1.0 }).is_err());
    assert_eq!(v.info_records().unwrap(), entry);
    // A sampler off the canvas reads nothing.
    v.info_command(InfoCommand::RemoveSampler(9)).unwrap();
    v.info_command(InfoCommand::AddSampler { x: -3.0, y: 2.0 }).unwrap();
    frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    assert_eq!(info(&v)["samplers"][9]["in_bounds"], false);
    // Canvas placement: with Place sampler on, a click adds a sampler at the pointer.
    v.info_command(InfoCommand::RemoveSampler(9)).unwrap();
    v.info_set_pick(Pick::Sampler);
    settle(&mut v, &ctx);
    let p = screen_of(&v, 12.0, 30.0);
    frame(&mut v, &ctx, vec![egui::Event::PointerMoved(p)], egui::Modifiers::NONE);
    frame(&mut v, &ctx, vec![egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }], egui::Modifiers::NONE);
    frame(&mut v, &ctx, vec![egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }], egui::Modifiers::NONE);
    frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    let s = info(&v)["samplers"].as_array().unwrap().last().cloned().unwrap();
    assert_eq!((info(&v)["samplers"].as_array().unwrap().len(), s["pixel"].clone()), (info::MAX_SAMPLERS, json!([12, 30])), "{}", v.state_json()["status"]);
}

#[test]
fn eyedropper_scope_reads_the_right_layers() {
    let (_s, mut v) = opened("eyedropper");
    let ctx = egui::Context::default();
    open_info(&mut v, &ctx);
    let (red_px, green_px) = ((10.0, 10.0), (50.0, 30.0));
    let red_frame = frame_sample(&v, 10, 10, SampleSize::Point).unwrap().rgba8;
    let green_frame = frame_sample(&v, 50, 30, SampleSize::Point).unwrap().rgba8;
    // No layer selected: layer scopes are refused, all_layers reads the composite.
    v.info_command(InfoCommand::SetSampleScope(SampleScope::CurrentLayer)).unwrap();
    assert!(v.info_eyedropper(green_px.0, green_px.1).unwrap_err().contains("select a layer"));
    let swatch = v.state_json()["edit"]["brush"]["rgb8"].clone();
    v.info_command(InfoCommand::SetSampleScope(SampleScope::AllLayers)).unwrap();
    let e = v.info_eyedropper(green_px.0, green_px.1).unwrap();
    assert_eq!(e["rgba8"], json!(green_frame));
    assert_eq!(v.state_json()["edit"]["brush"]["rgb8"], json!([green_frame[0], green_frame[1], green_frame[2]]));
    assert_ne!(v.state_json()["edit"]["brush"]["rgb8"], swatch);
    // Bottom layer selected: current_layer and current_and_below read red under the green layer.
    v.select(Some(layer_id(&v, "red")));
    for scope in [SampleScope::CurrentLayer, SampleScope::CurrentAndBelow] {
        v.info_command(InfoCommand::SetSampleScope(scope)).unwrap();
        let e = v.info_eyedropper(green_px.0, green_px.1).unwrap();
        assert_eq!((e["scope"].as_str(), e["rgba8"].clone()), (Some(scope.code()), json!(red_frame)), "{scope:?}");
    }
    // Top layer selected: current_layer reads green where it covers and nothing (transparent,
    // swatch unchanged) where it does not; current_and_below reads red there.
    v.select(Some(layer_id(&v, "green")));
    v.info_command(InfoCommand::SetSampleScope(SampleScope::CurrentLayer)).unwrap();
    assert_eq!(v.info_eyedropper(green_px.0, green_px.1).unwrap()["rgba8"], json!(green_frame));
    let before = v.state_json()["edit"]["brush"]["rgb8"].clone();
    assert!(v.info_eyedropper(red_px.0, red_px.1).unwrap_err().contains("eyedropper_transparent"));
    assert_eq!(v.state_json()["edit"]["brush"]["rgb8"], before);
    v.info_command(InfoCommand::SetSampleScope(SampleScope::CurrentAndBelow)).unwrap();
    assert_eq!(v.info_eyedropper(red_px.0, red_px.1).unwrap()["rgba8"], json!(red_frame));
    // Sample size averages across the layer edge (x = 32): 101x101 at (31, 24) with all layers
    // equals Pigment's sample of the frame.
    v.info_command(InfoCommand::SetSampleScope(SampleScope::AllLayers)).unwrap();
    v.info_command(InfoCommand::SetSampleSize(SampleSize::S101)).unwrap();
    let e = v.info_eyedropper(31.0, 24.0).unwrap();
    let want = frame_sample(&v, 31, 24, SampleSize::S101).unwrap();
    assert_eq!((e["rgba8"].clone(), e["pixels"].as_u64()), (json!(want.rgba8), Some(u64::from(W * H))));
    assert_eq!(info(&v)["eyedropper"]["rgba8"], json!(want.rgba8));
    // Outside the canvas: refused.
    assert!(v.info_eyedropper(-1.0, 3.0).unwrap_err().contains("eyedropper_outside_canvas"));
    // The document did not change (only entry steps were written).
    assert_eq!(v.layers().iter().filter(|r| r.visible).count(), 2);
}

#[test]
fn samplers_survive_save_and_reopen() {
    let (s, mut v) = opened("persist");
    v.info_command(InfoCommand::AddSampler { x: 3.0, y: 4.0 }).unwrap();
    v.info_command(InfoCommand::AddSampler { x: 60.5, y: 40.5 }).unwrap();
    v.info_command(InfoCommand::SetSamplerModel { index: 1, model: ColourModel::Lab }).unwrap();
    v.info_command(InfoCommand::SetSampleSize(SampleSize::S11)).unwrap();
    v.info_command(InfoCommand::SetSampleScope(SampleScope::CurrentAndBelow)).unwrap();
    v.info_command(InfoCommand::SetModel(ColourModel::Hsb)).unwrap();
    let records = v.info_records().unwrap();
    assert_eq!(records.samplers.len(), 2);
    let out = s.0.join("info.handshake");
    v.save(&out, false).unwrap();
    let mut w = crate::support_context::viewer(Prefer::Cpu);
    w.open(&out).unwrap();
    assert_eq!(w.info_records().unwrap(), records);
    w.render_if_dirty();
    let i = w.state_json()["info"].clone();
    assert_eq!((i["entry"].as_bool(), i["model"].as_str(), i["sample_size"].as_str(), i["scope"].as_str()), (Some(true), Some("hsb"), Some("11x11"), Some("current_and_below")));
    assert_eq!(i["samplers"][1]["readout"]["model"], "lab");
    assert_eq!(i["samplers"][1]["pixel"], json!([60, 40]));
    // Undo in the reopened document is empty for the entry; in the original it peels the steps.
    v.undo().unwrap();
    assert_eq!(v.info_records().unwrap().model, ColourModel::Rgb);
}

#[test]
fn hostile_info_entries_are_refused() {
    let ok = InfoRecords { samplers: vec![info::Sampler { x: 1.0, y: 2.0, model: ColourModel::Rgb }], ..Default::default() };
    assert_eq!(InfoRecords::parse(&ok.to_bytes()).unwrap(), ok);
    let base = ok.to_json();
    let mutate = |f: &dyn Fn(&mut Value)| {
        let mut v = base.clone();
        f(&mut v);
        InfoRecords::parse(v.to_string().as_bytes())
    };
    assert!(mutate(&|v| v["schema"] = json!("hsk.studio.harness.info@2")).unwrap_err().contains("schema"));
    assert!(mutate(&|v| v["extra"] = json!(1)).unwrap_err().contains("unknown_key"));
    assert!(mutate(&|v| v["model"] = json!("RGB")).unwrap_err().contains("model"));
    assert!(mutate(&|v| v["sample_size"] = json!("7x7")).unwrap_err().contains("sample_size"));
    assert!(mutate(&|v| v["scope"] = json!("layers")).unwrap_err().contains("scope"));
    assert!(mutate(&|v| v["samplers"][0]["x"] = json!(1e300)).unwrap_err().contains("coordinate"));
    assert!(mutate(&|v| v["samplers"][0]["x"] = json!("1")).unwrap_err().contains("missing: x"));
    assert!(mutate(&|v| v["samplers"][0]["extra"] = json!(0)).unwrap_err().contains("sampler_keys"));
    assert!(mutate(&|v| v["samplers"] = json!(vec![json!({"x": 0, "y": 0, "model": "rgb"}); info::MAX_SAMPLERS + 1])).unwrap_err().contains("too_many_samplers"));
    assert!(InfoRecords::parse(&vec![b' '; info::MAX_ENTRY_BYTES + 1]).unwrap_err().contains("too_large"));
    assert!(InfoRecords::parse(b"[1,2]").unwrap_err().contains("not_an_object"));
    assert!(InfoRecords::parse(&b"[".repeat(200_000)[..info::MAX_ENTRY_BYTES]).is_err());
    // Nothing open: commands are refused.
    let mut v = crate::support_context::viewer(Prefer::Cpu);
    assert!(v.info_command(InfoCommand::AddSampler { x: 0.0, y: 0.0 }).unwrap_err().contains("nothing open"));
    assert!(v.info_eyedropper(0.0, 0.0).is_err());
}
