//! SMB-HARNESS-DEVELOP-2 (feature `viewer`): the develop edit saved and restored through the session
//! document entry `develop`; panel families (kelvin / tint, colour grading, point colour, detail,
//! local corrections with luminance- and colour-range masks) as hsk-studio-develop types; hostile
//! entries and family values are typed refusals. Driven headlessly through the `Viewer` methods the
//! develop panel calls (no window, CX-SAFE-002); the DNG is the synthetic one of `common`.
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, synthetic_dng};
use hsk_studio_accord::CancellationToken;
use hsk_studio_develop::{BaselineExposure, DevelopError, OutputDepth, WbMode};
use hsk_studio_folio::Payload;
use hsk_studio_harness::view::develop::{DevelopExport, DevelopOp};
use hsk_studio_harness::view::develop_entry::{self, MAX_ENTRY_BYTES};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::{Viewer, widget};
use hsk_studio_harness::{Harness, References, Selection, default_actor};
use serde_json::{Value, json};
use std::path::Path;

fn digests(v: &Viewer) -> Vec<String> {
    let layer = v.develop_json()["layer_id"].as_str().unwrap().to_string();
    let snap = v.run().snapshot().unwrap();
    match &snap.document().layers.iter().find(|l| l.layer_id == layer).unwrap().payload {
        Payload::Raster { tiles } => tiles.iter().map(|t| t.content_digest.digest.clone()).collect(),
        _ => panic!("not raster"),
    }
}

fn open(path: &Path) -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    assert!(v.develop_available(), "{}", v.develop_json());
    v
}

fn ok(r: Result<u64, hsk_studio_harness::view::develop::Refusal>) -> u64 {
    r.unwrap_or_else(|e| panic!("refused: {e}"))
}

/// Every family through its own develop type, one undoable step each (pixels change), then save and
/// reopen: the `develop` entry restores the same edit and recipe with no loss note, and both
/// sessions export the same develop_to_output samples.
#[test]
fn develop_edit_saves_and_reopens_through_the_develop_entry() {
    let scratch = Scratch::new("develop2-save");
    let source = scratch.0.join("synthetic.dng");
    let bytes = synthetic_dng(300, 260);
    std::fs::write(&source, &bytes).unwrap();
    let mut v = open(&source);
    let imported = digests(&v);
    let mut last = imported.clone();
    macro_rules! step {
        ($what:expr, $call:expr) => {{
            ok($call);
            let now = digests(&v);
            assert_ne!(now, last, "{} changed no pixel", $what);
            last = now;
        }};
    }
    step!("kelvin", v.develop_op(DevelopOp::Kelvin { kelvin: 4300.0, tint: 12.0 }));
    step!("kelvin slider", v.set_develop("develop.wb_kelvin", 6100.0));
    step!("grading", v.set_develop_many(&[("develop.grading.shadows.hue", 220.0), ("develop.grading.shadows.saturation", 40.0), ("develop.grading.balance", -20.0)]));
    step!("point colour", v.develop_op(DevelopOp::AddPointColour { source: [0.30, 0.25, 0.20] }));
    step!("point shift", v.set_develop_many(&[("develop.point.0.hue_shift", 40.0), ("develop.point.0.saturation", 30.0)]));
    step!("detail", v.set_develop_many(&[("develop.sharpen_amount", 60.0), ("develop.sharpen_radius", 1.5), ("develop.texture", 25.0)]));
    ok(v.develop_op(DevelopOp::AddLuminanceRange { lo: 0.05, hi: 1.0, feather: 0.2 }));
    step!("luminance-range correction", v.set_develop("develop.local.0.exposure", 0.5));
    ok(v.develop_op(DevelopOp::AddColourRange { samples: vec![[0.30, 0.25, 0.20]], breadth: 0.6 }));
    step!("colour-range correction", v.set_develop("develop.local.1.saturation", -0.6));
    assert_ne!(last, imported);
    let state = v.develop_json();
    assert_eq!(state["enables"], json!({"enable_colour_adjustments": true, "enable_detail": true, "enable_mask_group_based_corrections": true}));
    assert_eq!(state["local"][0]["masks"][0], "luminance_range");
    assert_eq!(state["local"][1]["masks"][0], "colour_range");

    // The kelvin recipe is develop's own re-solve of the file's colour tags.
    let recipe = v.develop_recipe().unwrap();
    let raw = hsk_studio_develop::decode_container(&bytes).unwrap();
    let to_srgb = hsk_studio_develop::color::xyz_d50_to_linear_srgb().unwrap();
    let (direct, _) = raw.recipe_for_white(&to_srgb, recipe.working_profile.clone(), BaselineExposure::Ignore, 6100.0, 12.0).unwrap();
    assert_eq!((recipe.white_balance_mode, recipe.as_shot_neutral, recipe.camera_to_profile_rgb), (WbMode::Temperature, direct.as_shot_neutral, direct.camera_to_profile_rgb));

    let saved = scratch.0.join("saved.handshake");
    v.save(&saved, false).unwrap();
    let exported = v.export_develop(&scratch.0.join("a.png"), DevelopExport::Png { depth: OutputDepth::Sixteen }, false).unwrap();
    let edit = v.develop_edit().unwrap();

    let mut w = open(&saved);
    assert_eq!(w.run().ops()[0]["detail"]["loss_notes"], 0, "the develop entry is a known session entry: {}", w.run().ops()[0]);
    let state = w.develop_json();
    assert_eq!((state["entry"].as_str(), state["values_known"].as_bool()), (Some("Restored"), Some(true)), "{state}");
    assert_eq!(w.develop_edit().as_ref(), Some(&edit));
    assert_eq!(w.develop_recipe(), Some(recipe));
    let again = w.export_develop(&scratch.0.join("b.png"), DevelopExport::Png { depth: OutputDepth::Sixteen }, false).unwrap();
    assert_eq!(again["samples_sha256"], exported["samples_sha256"]);
    // The restored edit keeps working: one more step, undo returns to the restored pixels and edit.
    let restored = digests(&w);
    ok(w.develop_op(DevelopOp::RemoveCorrection(1)));
    assert_eq!(w.develop_json()["local"].as_array().map(Vec::len), Some(1));
    w.undo().unwrap();
    assert_eq!((digests(&w), w.develop_edit()), (restored, Some(edit)));
}

/// The panel draws every family with stable ids (headless egui frame), structure steps undo like
/// slider steps, and family values outside develop's ranges are develop's typed refusals.
#[test]
fn panel_families_are_drawn_undoable_and_refused_by_develop() {
    let scratch = Scratch::new("develop2-panel");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(64, 48)).unwrap();
    let mut v = open(&source);
    ok(v.develop_op(DevelopOp::Kelvin { kelvin: 5200.0, tint: 0.0 }));
    ok(v.develop_op(DevelopOp::AddPointColour { source: [0.4, 0.3, 0.2] }));
    ok(v.develop_op(DevelopOp::AddLuminanceRange { lo: 0.2, hi: 0.9, feather: 0.1 }));
    ok(v.develop_op(DevelopOp::AddColourRange { samples: vec![[0.4, 0.3, 0.2]], breadth: 0.4 }));
    let ctx = egui::Context::default();
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 1600.0))), ..Default::default() };
    let ops = v.run().ops().len();
    let _ = ctx.run(input, |ctx| v.ui(ctx));
    for name in [
        "develop.wb_kelvin", "develop.wb_tint", "develop.grading.midtones.hue", "develop.grading.blending", "develop.point.0.hue_shift",
        "develop.sharpen_radius", "develop.local.0.exposure", "develop.local.0.mask.0.lo", "develop.local.1.mask.0.breadth",
        widget::DEVELOP_AS_SHOT, widget::DEVELOP_ADD_POINT, widget::DEVELOP_ADD_LUMINANCE, widget::DEVELOP_ADD_COLOUR, "develop.point.0.remove", "develop.local.1.remove",
    ] {
        assert!(v.drawn().iter().any(|d| d == name), "{name} not drawn");
    }
    assert_eq!(v.run().ops().len(), ops, "drawing commits nothing");

    // Structure steps undo and redo through the chronicle history.
    let before = v.develop_edit().unwrap();
    ok(v.develop_op(DevelopOp::AsShotWhite));
    assert_eq!(v.develop_json()["white_balance"], "as_shot");
    v.undo().unwrap();
    assert_eq!(v.develop_edit().as_ref(), Some(&before));

    // Out-of-range family values: develop's codes, nothing committed.
    let (revision, undo) = (v.run().snapshot().unwrap().document().revision, v.run().history()["undo"].as_array().unwrap().len());
    let kelvin_code = hsk_studio_develop::white_xy(1000.0, 0.0).unwrap_err().code();
    let refusals: [(&str, f64, &str); 8] = [
        ("develop.wb_kelvin", 1000.0, kelvin_code),
        ("develop.grading.shadows.hue", 400.0, DevelopError::InvalidAdjustment.code()),
        ("develop.point.0.hue_range", 150.0, DevelopError::InvalidAdjustment.code()),
        ("develop.sharpen_radius", 9.0, DevelopError::InvalidAdjustment.code()),
        ("develop.local.0.exposure", 2.0, DevelopError::InvalidAdjustment.code()),
        ("develop.local.0.mask.0.lo", 0.95, DevelopError::InvalidMask.code()),
        ("develop.local.1.mask.0.breadth", f64::NAN, DevelopError::NonFinite.code()),
        ("develop.point.3.hue_shift", 1.0, "unknown_control"),
    ];
    for (id, value, code) in refusals {
        assert_eq!(v.set_develop(id, value).map_err(|r| r.code), Err(code), "{id} = {value}");
    }
    assert_eq!(v.develop_op(DevelopOp::RemoveCorrection(5)).unwrap_err().code, "unknown_item");
    assert_eq!(v.develop_op(DevelopOp::AddColourRange { samples: vec![], breadth: 0.4 }).unwrap_err().code, DevelopError::InvalidMask.code());
    assert_eq!(v.develop_op(DevelopOp::AddColourRange { samples: vec![[1.5, 0.0, 0.0]], breadth: 0.4 }).unwrap_err().code, DevelopError::InvalidMask.code());
    assert_eq!((v.run().snapshot().unwrap().document().revision, v.run().history()["undo"].as_array().unwrap().len()), (revision, undo));
    assert_eq!(v.develop_edit().as_ref(), Some(&before));
}

/// Hostile `develop` entries: the decoder refuses each with a typed code before anything is used; a
/// package carrying a refused entry still opens, with the file recipe and the refusal in the state.
#[test]
fn hostile_develop_entries_are_typed_refusals() {
    let scratch = Scratch::new("develop2-hostile");
    let source = scratch.0.join("synthetic.dng");
    let bytes = synthetic_dng(64, 48);
    std::fs::write(&source, &bytes).unwrap();
    let mut v = open(&source);
    ok(v.develop_op(DevelopOp::AddLuminanceRange { lo: 0.2, hi: 0.9, feather: 0.1 }));
    let layer = v.develop_json()["layer_id"].as_str().unwrap().to_string();
    let good = develop_entry::encode(&layer, None, &v.develop_edit().unwrap()).unwrap();
    let stored = develop_entry::decode(&good).unwrap();
    assert_eq!(Some(stored.edit), v.develop_edit(), "lossless round trip");
    let base: Value = serde_json::from_slice(&good).unwrap();
    let mutate = |f: &dyn Fn(&mut Value)| {
        let mut v = base.clone();
        f(&mut v);
        serde_json::to_vec(&v).unwrap()
    };
    let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("oversized", vec![b' '; MAX_ENTRY_BYTES + 1], "develop_entry_too_large"),
        ("not json", b"\x00\xffdevelop".to_vec(), "develop_entry_json"),
        ("deep nesting", deep.into_bytes(), "develop_entry_json"),
        ("number overflow", String::from_utf8(good.clone()).unwrap().replacen("\"exposure_stops\":0.0", "\"exposure_stops\":1e999", 1).into_bytes(), "develop_entry_json"),
        ("schema", mutate(&|v| v["schema"] = json!("hsk.studio.harness.develop@9")), "develop_entry_schema"),
        ("unknown key", mutate(&|v| v["edit"]["extra"] = json!(1)), "develop_entry_field"),
        ("missing key", mutate(&|v| { v["edit"]["tone"].as_object_mut().unwrap().remove("contrast"); }), "develop_entry_field"),
        ("string number", mutate(&|v| v["edit"]["tone"]["contrast"] = json!("10")), "develop_entry_field"),
        ("short mixer", mutate(&|v| { v["edit"]["mixer"].as_array_mut().unwrap().pop(); }), "develop_entry_field"),
        ("too many points", mutate(&|v| v["edit"]["point_colours"] = json!(vec![json!({"source":[0.1,0.1,0.1],"hue_shift":0.0,"saturation":0.0,"luminance":0.0,"hue_range":50.0,"saturation_range":50.0,"luminance_range":50.0}); 17])), "develop_entry_limit"),
        ("gradient mask", mutate(&|v| v["edit"]["local"][0]["masks"][0]["source"] = json!({"kind":"linear_gradient","zero":[0,0],"full":[1,1]})), "develop_entry_field"),
        ("bad signature", mutate(&|v| v["tiles_signature"] = json!("../../etc")), "develop_entry_field"),
        ("long id", mutate(&|v| v["edit"]["local"][0]["id"] = json!("x".repeat(300))), "develop_entry_field"),
    ];
    for (what, bytes, code) in &cases {
        assert_eq!(develop_entry::decode(bytes).map(|_| ()).map_err(|r| r.code), Err(*code), "{what}");
    }

    // Packages carrying a refused entry (one undecodable, one decodable but out of develop's range).
    let out_of_range = mutate(&|v| v["edit"]["tone"]["contrast"] = json!(500.0));
    for (name, entry, code) in [("junk", cases[1].1.clone(), "develop_entry_json"), ("range", out_of_range, DevelopError::InvalidAdjustment.code())] {
        let harness = Harness::new(Selection { providers: vec![], references: References::TrustAll }).unwrap();
        let cancel = CancellationToken::default();
        let mut run = harness.run(default_actor(), &cancel);
        assert!(run.open_import(&bytes));
        run.set_document_entry("develop", Some(entry)).unwrap();
        let path = scratch.0.join(format!("{name}.handshake"));
        assert!(run.save_to(&path), "{}", run.finish("save"));
        let w = open(&path);
        let state = w.develop_json();
        assert_eq!(state["entry"], format!("Refused({code:?})"), "{name}: {state}");
        assert_eq!(state["values"]["develop.contrast"], 0.0, "{name}: the panel starts from the file recipe");
    }
}
