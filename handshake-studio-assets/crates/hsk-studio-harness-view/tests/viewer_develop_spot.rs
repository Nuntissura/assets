//! Lane SMB-LR-HEALING-2 (feature `viewer`): spot removal in the window. The viewer switches
//! develop's `enable_retouch` / `enable_distraction_removal` on from the edit's lists (STU-RAW-105),
//! the typed spot ops are one undo step each through the develop re-develop + tiles commit, and the
//! develop panel's spot section turns preview clicks and drags into those ops (headless egui with
//! AccessKit; the layout is settled before any pointer event, F-TL-2). Oracles: the session's tile
//! digests and undo history, the rendered frame bytes, and `state_json()["develop"]`.
//! NOT_PROVEN: a spot tool on the main canvas (view/mod.rs is outside this lane); Lightroom's spot
//! tool behaviour (no Lightroom oracle, D5); a real desktop window.
#![cfg(feature = "viewer")]

#[path = "../../hsk-studio-harness/tests/common/mod.rs"]
mod common;

use common::{Scratch, synthetic_dng};
use hsk_studio_develop::SpotType;
use hsk_studio_folio::{Payload, TileRef};
use hsk_studio_harness_view::view::Viewer;
use hsk_studio_harness_view::view::develop::{DevelopOp, Refusal};
use hsk_studio_harness_view::view::render::Prefer;
use serde_json::Value;
use std::path::Path;

const W: u32 = 640;
const H: u32 = 320;

fn open(path: &Path) -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    assert!(v.develop_available(), "{}", v.develop_json());
    v
}

fn opened(label: &str) -> (Scratch, Viewer) {
    let scratch = Scratch::new(&format!("spot-{label}"));
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(W, H)).unwrap();
    let v = open(&source);
    (scratch, v)
}

fn tiles(v: &Viewer) -> Vec<TileRef> {
    let layer = v.develop_json()["layer_id"].as_str().unwrap().to_string();
    let snap = v.run().snapshot().unwrap();
    match &snap.document().layers.iter().find(|l| l.layer_id == layer).unwrap().payload {
        Payload::Raster { tiles } => tiles.clone(),
        _ => panic!("not raster"),
    }
}

fn digests(v: &Viewer) -> Vec<String> {
    tiles(v).iter().map(|t| t.content_digest.digest.clone()).collect()
}

fn ok(r: Result<u64, Refusal>) -> u64 {
    r.unwrap_or_else(|e| panic!("refused: {e}"))
}

fn undo_len(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().unwrap().len()
}

fn revision(v: &Viewer) -> u64 {
    v.run().snapshot().unwrap().document().revision
}

fn develop(v: &Viewer) -> Value {
    v.state_json()["develop"].clone()
}

/// Rendered frame pixel (RGBA8) at normalised `(x, y)`.
fn pixel(v: &mut Viewer, x: f64, y: f64) -> [u8; 4] {
    v.render_if_dirty();
    let f = v.frame().expect("frame");
    let (px, py) = ((x * f64::from(f.width)) as usize, (y * f64::from(f.height)) as usize);
    let at = (py * f.width as usize + px) * 4;
    f.rgba8[at..at + 4].try_into().unwrap()
}

fn clone_spot(centre: (f64, f64), source: (f64, f64)) -> DevelopOp {
    DevelopOp::AddSpot { spot_type: SpotType::Clone, centre, radius: 0.0625, source: Some(source), feather: 0.0, opacity: 1.0 }
}

// ---- slice 1: enable wiring and typed ops ----------------------------------------------------------

/// A spot stored in the edit renders: the viewer switches `enable_retouch` on, only the tiles the
/// spot covers change, and a saved package reopens with the same spot and the same pixels.
#[test]
fn stored_retouch_areas_now_render_in_the_window() {
    let (scratch, mut v) = opened("render");
    assert_eq!(develop(&v)["retouch_enables"]["enable_retouch"], false);
    let before = tiles(&v);
    // Spot at (480, 80) px, radius 0.0625 * 320 = 20 px, source 80 px to its left (exact dyadic coordinates).
    ok(v.develop_op(clone_spot((0.75, 0.25), (0.625, 0.25))));
    let d = develop(&v);
    assert_eq!(d["retouch_enables"]["enable_retouch"], true, "{d}");
    assert_eq!(d["retouch_enables"]["enable_distraction_removal"], false);
    assert_eq!(d["spots"][0]["spot_type"], "clone");
    assert_eq!(d["spots"][0]["source_state"], "source_set_explicitly");
    let after = tiles(&v);
    let changed: Vec<&TileRef> = after.iter().zip(&before).filter(|(a, b)| a.content_digest.digest != b.content_digest.digest).map(|(a, _)| a).collect();
    assert!(!changed.is_empty(), "the spot changed no tile");
    // Tiles are 256 px: the spot (460..500 x 60..100 px) lies in column 1, row 0 only.
    for t in &changed {
        assert_eq!((t.column, t.row), (1, 0), "tile ({}, {}) changed outside the spot", t.column, t.row);
    }
    let edit = v.develop_edit().unwrap();
    let pixels = digests(&v);
    let saved = scratch.0.join("saved.handshake");
    v.save(&saved, false).unwrap();
    let w = open(&saved);
    assert_eq!(w.develop_json()["entry"], "Current");
    assert_eq!(w.develop_edit(), Some(edit));
    assert_eq!(digests(&w), pixels);
    assert_eq!(w.develop_json()["retouch_enables"]["enable_retouch"], true);
}

/// Adding a heal spot is one undo step; undo restores the import pixels and the empty list.
#[test]
fn add_heal_spot_is_one_undo_step() {
    let (_s, mut v) = opened("undo");
    let (pixels, steps) = (digests(&v), undo_len(&v));
    ok(v.develop_op(DevelopOp::AddSpot { spot_type: SpotType::Heal, centre: (0.3, 0.4), radius: 0.05, source: Some((0.45, 0.4)), feather: 0.3, opacity: 0.8 }));
    assert_eq!(undo_len(&v), steps + 1);
    assert_ne!(digests(&v), pixels);
    let d = develop(&v);
    assert_eq!(d["spots"].as_array().unwrap().len(), 1);
    assert_eq!(d["spots"][0]["spot_type"], "heal");
    assert_eq!(d["spots"][0]["feather"], 0.3);
    for f in ["centre_x", "centre_y", "radius", "feather", "opacity", "source_x", "source_y"] {
        assert!(d["values"].get(format!("develop.spot.0.{f}")).is_some(), "control develop.spot.0.{f} missing");
    }
    v.undo().unwrap();
    assert_eq!(undo_len(&v), steps);
    assert_eq!(digests(&v), pixels);
    assert_eq!(develop(&v)["spots"].as_array().unwrap().len(), 0);
    v.redo().unwrap();
    assert_eq!(develop(&v)["spots"].as_array().unwrap().len(), 1);
}

/// A clone spot copies its source exactly (integer offset, no feather): the rendered target centre
/// equals the source centre; moving the source changes the copy, and the move is its own step.
#[test]
fn clone_spot_source_move_changes_the_copy() {
    let (_s, mut v) = opened("clone");
    ok(v.develop_op(clone_spot((0.5, 0.5), (0.25, 0.5))));
    let first = digests(&v);
    assert_eq!(pixel(&mut v, 0.5, 0.5), pixel(&mut v, 0.25, 0.5), "the clone copies the source");
    let steps = undo_len(&v);
    ok(v.develop_op(DevelopOp::MoveSpotSource { spot: 0, source: (0.75, 0.25) }));
    assert_eq!(undo_len(&v), steps + 1);
    assert_ne!(digests(&v), first, "moving the source changed no pixel");
    assert_eq!(pixel(&mut v, 0.5, 0.5), pixel(&mut v, 0.75, 0.25), "the clone follows the moved source");
    assert_eq!(develop(&v)["spots"][0]["source"], serde_json::json!([0.75, 0.25]));
    // The numeric controls address the same record.
    ok(v.set_develop("develop.spot.0.source_y", 0.375));
    assert_eq!(pixel(&mut v, 0.5, 0.5), pixel(&mut v, 0.75, 0.375));
}

/// A distraction spot fills through develop's native inpaint (pixels change, the distraction stage
/// switches on); undo restores the pixels; removal is a step too.
#[test]
fn distraction_spot_fills_and_undo_restores() {
    let (_s, mut v) = opened("distraction");
    let pixels = digests(&v);
    ok(v.develop_op(DevelopOp::AddDistraction { centre: (0.4, 0.5), radius: 0.04 }));
    let d = develop(&v);
    assert_eq!(d["retouch_enables"]["enable_distraction_removal"], true, "{d}");
    assert_eq!(d["retouch_enables"]["enable_retouch"], false);
    assert_eq!(d["distraction"][0]["centre"], serde_json::json!([0.4, 0.5]));
    let filled = digests(&v);
    assert_ne!(filled, pixels, "the fill changed no pixel");
    v.undo().unwrap();
    assert_eq!(digests(&v), pixels);
    v.redo().unwrap();
    assert_eq!(digests(&v), filled);
    ok(v.develop_op(DevelopOp::RemoveDistraction(0)));
    assert_eq!(digests(&v), pixels);
    assert_eq!(develop(&v)["retouch_enables"]["enable_distraction_removal"], false);
}

// ---- slice 2: the panel section ---------------------------------------------------------------------

fn input(events: Vec<egui::Event>, modifiers: egui::Modifiers) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 1200.0))), events, modifiers, ..Default::default() }
}

fn frame(v: &mut Viewer, ctx: &egui::Context, events: Vec<egui::Event>, modifiers: egui::Modifiers) -> egui::FullOutput {
    ctx.run(input(events, modifiers), |ctx| v.ui(ctx))
}

/// Frames until the preview rectangle repeats on two consecutive frames (F-TL-2).
fn settle(v: &mut Viewer, ctx: &egui::Context) {
    let mut last = Value::Null;
    for _ in 0..8 {
        let _ = frame(v, ctx, vec![], egui::Modifiers::NONE);
        let now = develop(v)["spot_preview"]["rect"].clone();
        if !now.is_null() && now == last {
            return;
        }
        last = now;
    }
    panic!("the spot preview did not settle: {}", develop(v)["spot_preview"]);
}

fn preview_rect(v: &Viewer) -> egui::Rect {
    let r = &develop(v)["spot_preview"]["rect"];
    let f = |i: usize| r[i].as_f64().unwrap() as f32;
    egui::Rect::from_min_max(egui::pos2(f(0), f(1)), egui::pos2(f(2), f(3)))
}

fn at(rect: egui::Rect, x: f64, y: f64) -> egui::Pos2 {
    egui::pos2(rect.min.x + x as f32 * rect.width(), rect.min.y + y as f32 * rect.height())
}

fn press(p: egui::Pos2, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers }
}

/// Press at `from`, move in steps to `to` over several frames, release.
fn drag(v: &mut Viewer, ctx: &egui::Context, from: egui::Pos2, to: egui::Pos2, modifiers: egui::Modifiers) {
    let _ = frame(v, ctx, vec![egui::Event::PointerMoved(from)], modifiers);
    let _ = frame(v, ctx, vec![press(from, true, modifiers)], modifiers);
    for k in 1..=6 {
        let t = k as f32 / 6.0;
        let _ = frame(v, ctx, vec![egui::Event::PointerMoved(from + (to - from) * t)], modifiers);
    }
    let _ = frame(v, ctx, vec![press(to, false, modifiers)], modifiers);
    let _ = frame(v, ctx, vec![], modifiers);
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

/// A click on the preview adds a heal spot (the default mode) at the pointer: one undo step, the
/// spot centre is the click point in normalised frame coordinates, with an explicit source.
#[test]
fn clicking_the_preview_adds_a_spot_at_the_pointer() {
    let (_s, mut v) = opened("click");
    let ctx = egui::Context::default();
    settle(&mut v, &ctx);
    assert!(v.drawn().iter().any(|d| d == "develop.spot.preview"), "preview not drawn");
    assert_eq!(develop(&v)["spot_preview"]["mode"], "heal");
    assert_eq!(develop(&v)["spot_preview"]["frame"], serde_json::json!([W, H]));
    let rect = preview_rect(&v);
    let steps = undo_len(&v);
    let p = at(rect, 0.3, 0.6);
    let _ = frame(&mut v, &ctx, vec![egui::Event::PointerMoved(p)], egui::Modifiers::NONE);
    let _ = frame(&mut v, &ctx, vec![press(p, true, egui::Modifiers::NONE)], egui::Modifiers::NONE);
    let _ = frame(&mut v, &ctx, vec![press(p, false, egui::Modifiers::NONE)], egui::Modifiers::NONE);
    let _ = frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    let d = develop(&v);
    assert_eq!(d["spots"].as_array().unwrap().len(), 1, "{}", v.state_json()["status"]);
    let s = &d["spots"][0];
    assert_eq!(s["spot_type"], "heal");
    let px = 1.0 / f64::from(rect.width());
    assert!(close(s["centre"][0].as_f64().unwrap(), 0.3, 2.0 * px) && close(s["centre"][1].as_f64().unwrap(), 0.6, 2.0 * px), "{s}");
    assert_eq!(s["radius"], 0.05);
    assert_eq!(s["source_state"], "source_set_explicitly");
    assert_eq!(undo_len(&v), steps + 1);
    // A drag from an empty point places a spot with the drag length as its radius.
    settle(&mut v, &ctx);
    let rect = preview_rect(&v);
    drag(&mut v, &ctx, at(rect, 0.7, 0.5), at(rect, 0.7, 0.5) + egui::vec2(rect.height() * 0.1, 0.0), egui::Modifiers::NONE);
    let d = develop(&v);
    assert_eq!(d["spots"].as_array().unwrap().len(), 2, "{}", v.state_json()["status"]);
    let r = d["spots"][1]["radius"].as_f64().unwrap();
    assert!(close(r, 0.1, 0.02), "dragged radius {r}");
}

/// Dragging a spot's source handle moves its source (one step); Alt+drag from inside a spot does too.
#[test]
fn dragging_the_source_handle_moves_the_source() {
    let (_s, mut v) = opened("source");
    ok(v.develop_op(clone_spot((0.5, 0.5), (0.25, 0.5))));
    let ctx = egui::Context::default();
    settle(&mut v, &ctx);
    let rect = preview_rect(&v);
    let steps = undo_len(&v);
    drag(&mut v, &ctx, at(rect, 0.25, 0.5), at(rect, 0.8, 0.3), egui::Modifiers::NONE);
    let d = develop(&v);
    assert_eq!(d["spots"].as_array().unwrap().len(), 1, "a handle drag must not add a spot");
    let s = &d["spots"][0];
    let px = 1.0 / f64::from(rect.width());
    assert!(close(s["source"][0].as_f64().unwrap(), 0.8, 2.0 * px) && close(s["source"][1].as_f64().unwrap(), 0.3, 2.0 * px), "{s}");
    assert_eq!(undo_len(&v), steps + 1);
    assert_ne!(develop(&v)["values"]["develop.spot.0.source_x"], 0.25, "the record moved");
    // Alt + drag from the target circle sets the source too.
    settle(&mut v, &ctx);
    drag(&mut v, &ctx, at(rect, 0.5, 0.5), at(rect, 0.2, 0.7), egui::Modifiers::ALT);
    let s = develop(&v)["spots"][0].clone();
    assert_eq!(develop(&v)["spots"].as_array().unwrap().len(), 1);
    assert!(close(s["source"][0].as_f64().unwrap(), 0.2, 2.0 * px) && close(s["source"][1].as_f64().unwrap(), 0.7, 2.0 * px), "{s}");
}

/// The section's controls are AccessKit nodes: the mode buttons switch the mode, the spot row's
/// type button and remove button act through their nodes (feature `desktop`: egui AccessKit).
#[cfg(feature = "desktop")]
#[test]
fn spot_controls_are_accesskit_nodes() {
    let (_s, mut v) = opened("accesskit");
    ok(v.develop_op(clone_spot((0.5, 0.5), (0.25, 0.5))));
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    settle(&mut v, &ctx);
    let find = |out: &egui::FullOutput, label: &str| -> egui::accesskit::NodeId {
        let tree = out.platform_output.accesskit_update.as_ref().expect("accesskit tree");
        tree.nodes.iter().find(|(_, n)| n.label().is_some_and(|l| l == label)).map(|(id, _)| *id).unwrap_or_else(|| panic!("no accesskit node {label}"))
    };
    let click = |id| egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest { action: egui::accesskit::Action::Click, target: id, data: None });
    let out = frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    find(&out, "Spot preview");
    let distraction = find(&out, "Spot mode distraction");
    let _ = frame(&mut v, &ctx, vec![click(distraction)], egui::Modifiers::NONE);
    let _ = frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    assert_eq!(develop(&v)["spot_preview"]["mode"], "distraction");
    let out = frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    let switch = find(&out, "Spot 0 clone to heal");
    let _ = frame(&mut v, &ctx, vec![click(switch)], egui::Modifiers::NONE);
    let _ = frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    assert_eq!(develop(&v)["spots"][0]["spot_type"], "heal");
    let out = frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    let remove = find(&out, "Remove spot 0");
    let _ = frame(&mut v, &ctx, vec![click(remove)], egui::Modifiers::NONE);
    let _ = frame(&mut v, &ctx, vec![], egui::Modifiers::NONE);
    assert_eq!(develop(&v)["spots"].as_array().unwrap().len(), 0);
    for name in ["develop.spot.mode.heal", "develop.spot.mode.clone", "develop.spot.mode.distraction", "develop.spot.preview"] {
        assert!(v.drawn().iter().any(|d| d == name), "{name} not drawn");
    }
}

/// Hostile spot values are develop's typed refusals (or this module's for unknown items); a refused
/// change commits nothing.
#[test]
fn hostile_spot_values_are_develop_refusals() {
    let (_s, mut v) = opened("hostile");
    let (pixels, steps, rev) = (digests(&v), undo_len(&v), revision(&v));
    let spot = |radius: f64, source: Option<(f64, f64)>, opacity: f64| DevelopOp::AddSpot { spot_type: SpotType::Heal, centre: (0.5, 0.5), radius, source, feather: 0.0, opacity };
    let cases: Vec<(DevelopOp, &str)> = vec![
        (spot(f64::NAN, Some((0.2, 0.5)), 1.0), "non_finite"),
        (spot(0.05, Some((f64::INFINITY, 0.5)), 1.0), "non_finite"),
        (spot(2.0, Some((0.2, 0.5)), 1.0), "invalid_mask"),
        (spot(0.0, Some((0.2, 0.5)), 1.0), "invalid_mask"),
        (spot(0.05, Some((0.2, 0.5)), 1.5), "invalid_mask"),
        (spot(0.05, Some((40.0, 0.5)), 1.0), "invalid_mask"),
        (spot(0.2, Some((0.98, 0.5)), 1.0), "unsupported_contribution"),
        (DevelopOp::MoveSpotSource { spot: 3, source: (0.5, 0.5) }, "unknown_item"),
        (DevelopOp::RemoveSpot(0), "unknown_item"),
        (DevelopOp::RemoveDistraction(0), "unknown_item"),
        (DevelopOp::SetSpot { spot: 0, spot_type: SpotType::Clone, feather: 0.0, opacity: 1.0, radius: 0.1 }, "unknown_item"),
        (DevelopOp::AddDistraction { centre: (0.5, f64::NAN), radius: 0.05 }, "non_finite"),
        (DevelopOp::AddDistraction { centre: (0.5, 0.5), radius: -1.0 }, "invalid_mask"),
    ];
    for (op, code) in cases {
        let label = format!("{op:?}");
        let r = v.develop_op(op).expect_err(&label);
        assert_eq!(r.code, code, "{label}: {r}");
        assert_eq!((digests(&v), undo_len(&v), revision(&v)), (pixels.clone(), steps, rev), "{label} committed");
    }
    // Controls of a valid spot: out-of-range values are develop's refusals, unknown fields unknown.
    ok(v.develop_op(spot(0.05, Some((0.3, 0.5)), 1.0)));
    let (pixels, steps) = (digests(&v), undo_len(&v));
    for (id, value, code) in [
        ("develop.spot.0.opacity", 1.5, "invalid_mask"),
        ("develop.spot.0.feather", -0.1, "invalid_mask"),
        ("develop.spot.0.radius", f64::NAN, "non_finite"),
        ("develop.spot.0.seed", 1.0, "unknown_control"),
        ("develop.spot.1.radius", 0.1, "unknown_control"),
        ("develop.distraction.0.radius", 0.1, "unknown_control"),
    ] {
        let r = v.set_develop(id, value).expect_err(id);
        assert_eq!(r.code, code, "{id}: {r}");
        assert_eq!((digests(&v), undo_len(&v)), (pixels.clone(), steps), "{id} committed");
    }
}
