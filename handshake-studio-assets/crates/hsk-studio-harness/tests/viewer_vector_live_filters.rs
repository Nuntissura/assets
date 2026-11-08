//! Lane SMB-AI-LIVE-EFFECTS-1 (feature `viewer`): vector live effects as typed commands through the
//! drive path (`Viewer::run_command`, the call the process drive surface makes): the STU-FX-007 verbs
//! `vector.live_filter_apply|update|reorder|toggle|remove|bake|read` over a vector layer's Folio
//! live filter stack. Each mutation is ONE undo step (STU-FX-012c); the render re-evaluates from the
//! source paths (STU-FX-027); removing an entry restores the exact prior render (STU-FX-003); the
//! same seed gives byte-identical output (STU-FX-011); save, reopen and bake keep the render. Written
//! against `run_command` and JSON, so the red commit compiles and fails while the rows are missing.
//! Expected pixels are hand-derived (document px, 1 px = 1 pixel). Laws are nib's Studio readings v1;
//! vendor parity UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::arrange::ReferencePoint;
use hsk_studio_harness::view::vector::{EnvelopeSpec, ShapeKind, TransformOp, VectorTool};
use hsk_studio_folio::{StudioWarpAxis, StudioWarpStyle};
use serde_json::{Value, json};

const W: usize = 96;
const H: usize = 64;
const WHITE: [u8; 3] = [255, 255, 255];
const RED: [u8; 3] = [255, 0, 0];
const BLUE: [u8; 3] = [0, 0, 255];

fn paper(scratch: &Scratch) -> Viewer {
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(W as u32, H as u32, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: true, fill: RED, stroke_on: false, stroke: BLUE, stroke_width: 2.0, ..VectorTool::default() });
    v
}
fn frame(v: &mut Viewer) -> Vec<u8> {
    v.render_if_dirty();
    v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).rgba8.clone()
}
fn px(f: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * W + x) * 4;
    [f[i], f[i + 1], f[i + 2]]
}
/// One typed command through the drive path; `Err` is the refusal code.
fn cmd(v: &mut Viewer, id: &str, params: Value) -> Result<Value, String> {
    v.run_command(id, params.as_object().expect("an object")).map_err(|r| format!("{} {}", r.code, r.detail))
}
fn real(key: &str, v: f64) -> Value {
    json!({"type":"Real","key":key,"value":v})
}
fn int(key: &str, v: i64) -> Value {
    json!({"type":"Int","key":key,"value":v})
}
fn token(key: &str, v: &str) -> Value {
    json!({"type":"String","key":key,"value":v})
}
fn kind(s: &str) -> String {
    format!("hsk.studio.vector_effect.{s}")
}
fn apply(v: &mut Viewer, layer: &str, effect: &str, params: Vec<Value>) -> Result<String, String> {
    cmd(v, "vector.live_filter_apply", json!({"layer":layer,"filter_kind":kind(effect),"params":params})).map(|r| r.as_str().unwrap_or_default().to_string())
}
fn read(v: &mut Viewer, layer: &str) -> Value {
    cmd(v, "vector.live_filter_read", json!({"layer":layer})).unwrap()
}
fn roughen(seed: i64) -> Vec<Value> {
    vec![token("roughen_mode", "mode.absolute"), real("roughen_size_absolute", 3.0), real("roughen_detail", 20.0), token("roughen_points", "points.corner"), int("seed", seed)]
}

#[test]
fn live_filters_apply_undo_follow_the_source_reorder_and_remove() {
    let scratch = Scratch::new("live-filters-edit");
    let mut v = paper(&scratch);
    let r = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [30.0, 20.0], false).unwrap();
    let plain = frame(&mut v);
    assert_eq!((px(&plain, 20, 15), px(&plain, 60, 15)), (RED, WHITE));
    // Step repeat: one copy 40 px to the right; the source stays.
    let step = apply(&mut v, &r, "step_repeat", vec![int("copies", 1), real("move_horizontal", 40.0)]).unwrap();
    assert!(!step.is_empty());
    let live = frame(&mut v);
    assert_eq!((px(&live, 20, 15), px(&live, 60, 15)), (RED, RED), "apply draws the copy");
    // ONE undo step; undo restores the exact bytes, redo the effect.
    v.undo().unwrap();
    assert_eq!(frame(&mut v), plain);
    v.redo().unwrap();
    assert_eq!(frame(&mut v), live);
    // Editing the source re-renders the effect from it (STU-FX-027): the copy follows the move.
    v.transform_layers(std::slice::from_ref(&r), TransformOp::Move { dx: 10.0, dy: 0.0 }, ReferencePoint::Centre).unwrap();
    let moved = frame(&mut v);
    assert_eq!((px(&moved, 15, 15), px(&moved, 35, 15), px(&moved, 55, 15), px(&moved, 75, 15)), (WHITE, RED, WHITE, RED));
    v.undo().unwrap();
    assert_eq!(frame(&mut v), live);
    // A second entry; reorder changes the output deterministically and back again restores it.
    let twist = apply(&mut v, &r, "twist", vec![real("twist_angle", 40.0)]).unwrap();
    let a = frame(&mut v);
    assert_ne!(a, live);
    cmd(&mut v, "vector.live_filter_reorder", json!({"layer":r,"instance":twist,"to":0})).unwrap();
    let b = frame(&mut v);
    assert_ne!(b, a, "twist then step repeat is another picture");
    cmd(&mut v, "vector.live_filter_reorder", json!({"layer":r,"instance":twist,"to":1})).unwrap();
    assert_eq!(frame(&mut v), a);
    // Update one typed key; a wrong type or a value past the hard range is refused before commit.
    cmd(&mut v, "vector.live_filter_update", json!({"layer":r,"instance":twist,"param":real("twist_angle", -40.0)})).unwrap();
    let c = frame(&mut v);
    assert_ne!(c, a);
    let wrong = cmd(&mut v, "vector.live_filter_update", json!({"layer":r,"instance":twist,"param":int("twist_angle", 3)}));
    assert!(wrong.as_ref().is_err_and(|e| e.contains("vector_effect_wrong_parameter_type")), "{wrong:?}");
    let far = cmd(&mut v, "vector.live_filter_update", json!({"layer":r,"instance":step,"param":real("move_horizontal", 4000.5)}));
    assert!(far.as_ref().is_err_and(|e| e.contains("vector_effect_out_of_hard_range")), "{far:?}");
    let untyped = cmd(&mut v, "vector.live_filter_update", json!({"layer":r,"instance":twist,"param":{"type":"Float","key":"twist_angle","value":1.0}}));
    assert!(untyped.as_ref().is_err_and(|e| e.starts_with("parameter_invalid")), "{untyped:?}");
    assert_eq!(frame(&mut v), c, "refused writes leave the render");
    // Toggle off draws as without the entry; on again restores.
    cmd(&mut v, "vector.live_filter_toggle", json!({"layer":r,"instance":twist,"enabled":false})).unwrap();
    assert_eq!(frame(&mut v), live);
    cmd(&mut v, "vector.live_filter_toggle", json!({"layer":r,"instance":twist,"enabled":true})).unwrap();
    assert_eq!(frame(&mut v), c);
    // Remove restores the exact prior render (STU-FX-003).
    cmd(&mut v, "vector.live_filter_remove", json!({"layer":r,"instance":twist})).unwrap();
    assert_eq!(frame(&mut v), live);
    cmd(&mut v, "vector.live_filter_remove", json!({"layer":r,"instance":step})).unwrap();
    assert_eq!(frame(&mut v), plain);
    assert_eq!(read(&mut v, &r)["filters"], json!([]));
}

#[test]
fn the_same_seed_gives_the_same_bytes_and_the_read_lists_every_row() {
    let scratch = Scratch::new("live-filters-seed");
    let mut v = paper(&scratch);
    let r = v.draw_shape(ShapeKind::Rectangle, [20.0, 20.0], [60.0, 40.0], false).unwrap();
    let id = apply(&mut v, &r, "roughen", roughen(7)).unwrap();
    let seven = frame(&mut v);
    cmd(&mut v, "vector.live_filter_remove", json!({"layer":r,"instance":id})).unwrap();
    apply(&mut v, &r, "roughen", roughen(7)).unwrap();
    assert_eq!(frame(&mut v), seven, "same record, same seed: byte-identical (STU-FX-011)");
    let id = read(&mut v, &r)["filters"][0]["instance_id"].as_str().unwrap().to_string();
    cmd(&mut v, "vector.live_filter_update", json!({"layer":r,"instance":id,"param":int("seed", 8)})).unwrap();
    assert_ne!(frame(&mut v), seven);
    // The read: every table row with its seven fields beside the stored value (STU-VEC-154).
    let filters = read(&mut v, &r)["filters"].clone();
    let rows = filters[0]["params"].as_array().unwrap().clone();
    let row = |key: &str| rows.iter().find(|p| p["key"] == key).cloned().unwrap_or_else(|| panic!("{key}"));
    let size = row("roughen_size_absolute");
    assert_eq!((size["hard_min"].clone(), size["hard_max"].clone(), size["soft_max"].clone(), size["unit"].clone(), size["value"].clone()), (json!(0.0), json!(7200.0), json!(100.0), json!("document_unit"), json!(3.0)));
    assert_eq!(row("roughen_mode")["members"][1]["id"], "mode.absolute");
    assert_eq!(row("seed")["value"], json!(8));
    assert_eq!((filters[0]["enabled"].clone(), filters[0]["version"].clone(), filters[0]["known"].clone()), (json!(true), json!(1), json!(true)));
    // Every resolved entry is stored, defaults included: a fresh step repeat stores scale 100.
    let s = v.draw_shape(ShapeKind::Rectangle, [70.0, 5.0], [80.0, 10.0], false).unwrap();
    apply(&mut v, &s, "step_repeat", vec![]).unwrap();
    let stored = read(&mut v, &s)["filters"][0]["params"].clone();
    let scale = stored.as_array().unwrap().iter().find(|p| p["key"] == "scale_horizontal").cloned().unwrap();
    assert_eq!((scale["value"].clone(), scale["default_is_studio_decision"].clone(), scale["provenance"].clone()), (json!(100.0), json!(true), json!({"recovered":"STU-VEC-157"})));
}

#[test]
fn save_reopen_and_bake_keep_the_render() {
    let scratch = Scratch::new("live-filters-save");
    let mut v = paper(&scratch);
    // An active artboard: the framed render path (stale fact of the 2026-11-08 rulings).
    let board = v.add_artboard("Board", W as f64, H as f64, Some([0.0, 0.0])).unwrap();
    v.go_to_artboard(&board).unwrap();
    let r = v.draw_shape(ShapeKind::Rectangle, [20.0, 20.0], [60.0, 40.0], false).unwrap();
    apply(&mut v, &r, "roughen", roughen(3)).unwrap();
    apply(&mut v, &r, "step_repeat", vec![int("copies", 1), real("move_vertical", 12.0)]).unwrap();
    // An envelope stays through the bake.
    v.make_envelope(&r, &EnvelopeSpec::Warp { style: StudioWarpStyle::Arc, axis: StudioWarpAxis::Horizontal, bend: 10.0, horizontal: 0.0, vertical: 0.0 }, 50.0, false).unwrap();
    let live = frame(&mut v);
    let out = scratch.0.join("live.handshake");
    v.save(&out, false).unwrap();
    let mut again = Viewer::new(Prefer::Cpu);
    again.open(&out).unwrap();
    again.go_to_artboard(&board).unwrap();
    assert_eq!(frame(&mut again), live, "save and reopen keep the stack");
    assert_eq!(read(&mut again, &r)["filters"].as_array().map(Vec::len), Some(2));
    // Bake: the effected paths become the layer's paths, the stack goes, the envelope stays; ONE step.
    cmd(&mut again, "vector.live_filter_bake", json!({"layer":r})).unwrap();
    assert_eq!(read(&mut again, &r)["filters"], json!([]));
    assert!(again.envelope(&r).unwrap().is_some());
    assert_eq!(frame(&mut again), live, "bake then render equals the live render");
    again.undo().unwrap();
    assert_eq!(read(&mut again, &r)["filters"].as_array().map(Vec::len), Some(2));
    assert_eq!(frame(&mut again), live);
}

#[test]
fn free_distort_corners_follow_the_layer_and_hostile_commands_are_refused() {
    let scratch = Scratch::new("live-filters-hostile");
    let mut v = paper(&scratch);
    let r = v.draw_shape(ShapeKind::Rectangle, [20.0, 20.0], [60.0, 40.0], false).unwrap();
    let mut params = Vec::new();
    for (prefix, q) in [("src", [[20.0, 20.0], [60.0, 20.0], [60.0, 40.0], [20.0, 40.0]]), ("dst", [[15.0, 18.0], [65.0, 20.0], [58.0, 44.0], [22.0, 38.0]])] {
        for (i, [h, vv]) in q.iter().enumerate() {
            params.push(real(&format!("{prefix}{i}_h"), *h));
            params.push(real(&format!("{prefix}{i}_v"), *vv));
        }
    }
    apply(&mut v, &r, "free_distort", params).unwrap();
    let before = frame(&mut v);
    // The corners are document coordinates: a moved layer distorts against a moved frame.
    v.transform_layers(std::slice::from_ref(&r), TransformOp::Move { dx: 10.0, dy: 5.0 }, ReferencePoint::Centre).unwrap();
    let rows = read(&mut v, &r)["filters"][0]["params"].clone();
    let value = |key: &str| rows.as_array().unwrap().iter().find(|p| p["key"] == key).map(|p| p["value"].clone()).unwrap();
    assert_eq!((value("src0_h"), value("src0_v"), value("dst2_h"), value("dst2_v")), (json!(30.0), json!(25.0), json!(68.0), json!(49.0)));
    v.undo().unwrap();
    assert_eq!(frame(&mut v), before);
    // Refused before any commit: unknown effect, unset parameter, a law nib does not evaluate, a
    // raster layer, a missing instance, a reorder past the stack.
    let refusals: Vec<(Result<String, String>, &str)> = vec![
        (apply(&mut v, &r, "scribble", vec![]), "vector_effect_unknown_effect"),
        (apply(&mut v, &r, "twist", vec![]), "vector_effect_parameter_unset"),
        (apply(&mut v, &r, "convert_to_shape", vec![token("target_shape", "shape.ellipse")]), "vector_effect_convert_to_shape_sizing_law_not_recovered"),
        (apply(&mut v, &r, "step_repeat", vec![int("copies", i64::MAX)]), "vector_effect_output_over_budget"),
        (cmd(&mut v, "vector.live_filter_remove", json!({"layer":r,"instance":"lf99"})).map(|_| String::new()), "no entry lf99"),
        (cmd(&mut v, "vector.live_filter_reorder", json!({"layer":r,"instance":"lf1","to":5})).map(|_| String::new()), "past the stack"),
    ];
    for (got, want) in refusals {
        assert!(got.as_ref().is_err_and(|e| e.contains(want)), "{want}: {got:?}");
    }
    let paper_layer = v.layers().into_iter().map(|l| l.layer_id).find(|id| id != &r).unwrap();
    let raster = apply(&mut v, &paper_layer, "twist", vec![real("twist_angle", 10.0)]);
    assert!(raster.as_ref().is_err_and(|e| e.contains("not a vector layer")), "{raster:?}");
    assert_eq!(frame(&mut v), before, "the render is unchanged");
}

#[test]
fn a_duplicated_layer_does_not_carry_the_stack_pinned() {
    // V4-06 sweep (brief item 5): `duplicate_layer` copies the layer record only; the vector entry's
    // per-layer records (live filters, envelope) stay with the original. Pinned here, reported to
    // the lead; changing it is a later slice (the copy would need an entry write in the same step).
    let scratch = Scratch::new("live-filters-duplicate");
    let mut v = paper(&scratch);
    let r = v.draw_shape(ShapeKind::Rectangle, [20.0, 20.0], [60.0, 40.0], false).unwrap();
    apply(&mut v, &r, "twist", vec![real("twist_angle", 30.0)]).unwrap();
    v.make_envelope(&r, &EnvelopeSpec::Warp { style: StudioWarpStyle::Arc, axis: StudioWarpAxis::Horizontal, bend: 10.0, horizontal: 0.0, vertical: 0.0 }, 50.0, false).unwrap();
    let copy = v.duplicate_layer(&r).unwrap();
    assert_eq!(read(&mut v, &copy)["filters"], json!([]), "the copy has no live filters");
    assert!(v.envelope(&copy).unwrap().is_none(), "the copy has no envelope");
    assert_eq!(read(&mut v, &r)["filters"].as_array().map(Vec::len), Some(1), "the original keeps its stack");
}

#[test]
fn a_transform_that_would_store_a_non_finite_corner_is_refused_before_commit() {
    // Review F2: a disabled free distort never meets the trial evaluation, and a scale of a huge
    // stored corner would write a non-finite value (serialised as null) and brick the entry.
    let scratch = Scratch::new("live-filters-corner");
    let mut v = paper(&scratch);
    let r = v.draw_shape(ShapeKind::Rectangle, [20.0, 20.0], [60.0, 40.0], false).unwrap();
    let mut params = Vec::new();
    for (prefix, q) in [("src", [[20.0, 20.0], [60.0, 20.0], [60.0, 40.0], [20.0, 40.0]]), ("dst", [[15.0, 18.0], [65.0, 20.0], [58.0, 44.0], [22.0, 38.0]])] {
        for (i, [h, vv]) in q.iter().enumerate() {
            params.push(real(&format!("{prefix}{i}_h"), *h));
            params.push(real(&format!("{prefix}{i}_v"), *vv));
        }
    }
    let id = apply(&mut v, &r, "free_distort", params).unwrap();
    cmd(&mut v, "vector.live_filter_toggle", json!({"layer":r,"instance":id,"enabled":false})).unwrap();
    cmd(&mut v, "vector.live_filter_update", json!({"layer":r,"instance":id,"param":real("src0_h", 1.0e308)})).unwrap();
    let before = frame(&mut v);
    let scaled = v.transform_layers(std::slice::from_ref(&r), TransformOp::Scale { horizontal: 200.0, vertical: 200.0 }, ReferencePoint::Centre);
    assert!(scaled.as_ref().is_err_and(|e| e.contains("vector_effect_beyond_engine_range")), "{scaled:?}");
    // Nothing was committed: the entry still decodes and holds the corner unchanged.
    let rows = read(&mut v, &r)["filters"][0]["params"].clone();
    let corner = rows.as_array().unwrap().iter().find(|p| p["key"] == "src0_h").map(|p| p["value"].clone());
    assert_eq!(corner, Some(json!(1.0e308)));
    assert_eq!(frame(&mut v), before);
}

#[test]
fn expand_envelope_refuses_while_the_layer_holds_a_live_filter_stack() {
    // Review F4 (STU-VEC-054, STU-FX-003): the render is envelope(filters(source)); expanding only the
    // envelope from the raw paths would leave filters(envelope(source)). Refused typed while any
    // stack is on the layer, enabled or not; bake or remove the filters first.
    let scratch = Scratch::new("live-filters-envelope");
    let mut v = paper(&scratch);
    let r = v.draw_shape(ShapeKind::Rectangle, [20.0, 20.0], [60.0, 40.0], false).unwrap();
    let id = apply(&mut v, &r, "roughen", roughen(3)).unwrap();
    v.make_envelope(&r, &EnvelopeSpec::Warp { style: StudioWarpStyle::Arc, axis: StudioWarpAxis::Horizontal, bend: 10.0, horizontal: 0.0, vertical: 0.0 }, 50.0, false).unwrap();
    let live = frame(&mut v);
    let expanded = v.expand_envelope(&r);
    assert!(expanded.as_ref().is_err_and(|e| e.contains("live_filter")), "{expanded:?}");
    assert!(v.envelope(&r).unwrap().is_some());
    assert_eq!(frame(&mut v), live);
    cmd(&mut v, "vector.live_filter_toggle", json!({"layer":r,"instance":id,"enabled":false})).unwrap();
    assert!(v.expand_envelope(&r).is_err(), "a disabled entry still holds the stack");
    // Bake first, then the envelope expands and the picture is kept.
    cmd(&mut v, "vector.live_filter_toggle", json!({"layer":r,"instance":id,"enabled":true})).unwrap();
    cmd(&mut v, "vector.live_filter_bake", json!({"layer":r})).unwrap();
    assert_eq!(frame(&mut v), live);
    v.expand_envelope(&r).unwrap();
    assert!(v.envelope(&r).unwrap().is_none());
}
