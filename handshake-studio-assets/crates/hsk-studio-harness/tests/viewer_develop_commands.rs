//! Lane SMB-DEVELOP-COMMANDS-1 (feature `viewer`): every develop operation of the Viewer is a typed
//! `photo.develop.*` row. Each row is checked against a direct `Viewer::develop_op` (or `set_develop`)
//! call with the same operation on a second viewer of the same raw: same tile digests, same
//! `develop_json`, one undo step each (a differential, so a decoder cannot drift from the op). One
//! process test runs scenario photo-retouch steps 27-30, 40, 41, 46 and 51 through `command --stdin`.
//! The develop must-haves (group enables, point curves, crop and straighten, snapshots, preset save) are
//! state in the develop entry, one undo step each, and reopen; the entry decoder bounds them.
//! Headless (no window, CX-SAFE-002). Lightroom and Camera Raw parity UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd, synthetic_dng};
use hsk_studio_develop::{Brush, ColourRange, LookDomain, LuminanceRange, MaskSource, RadialGradient, UprightMode, VignetteStyle};
use hsk_studio_folio::Payload;
use hsk_studio_harness::cli::{self, WindowArgs};
use hsk_studio_harness::view::commands::CommandRefusal;
use hsk_studio_develop::{NormalizedCrop, OutputDepth};
use hsk_studio_harness::view::develop::{CurveChannel, DevelopExport, DevelopOp, RecipeOp, Refusal, default_linear};
use hsk_studio_harness_core::action;
use hsk_studio_harness::view::develop_entry;
use hsk_studio_harness::view::develop_more::FamilyOp;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::Viewer;
use serde_json::{Map, Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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

fn undo_len(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().unwrap().len()
}

fn row(v: &mut Viewer, id: &str, params: Value) -> Result<Value, CommandRefusal> {
    let map: Map<String, Value> = params.as_object().cloned().unwrap_or_default();
    v.run_command(id, &map)
}

/// The row on `a` and the direct call on `b` both succeed, make one undo step each, and leave the
/// same pixels and the same develop state.
fn same(a: &mut Viewer, b: &mut Viewer, id: &str, params: Value, direct: impl FnOnce(&mut Viewer) -> Result<u64, Refusal>) {
    let steps = undo_len(a);
    let typed = row(a, id, params);
    let called = direct(b);
    assert!(typed.is_ok(), "{id}: the row refused {typed:?} (direct {called:?})");
    assert!(called.is_ok(), "{id}: the direct call refused {called:?}");
    assert_eq!(undo_len(a), steps + 1, "{id} is one undo step");
    assert_eq!(undo_len(a), undo_len(b), "{id}: undo steps differ");
    assert_eq!(digests(a), digests(b), "{id}: tiles differ from the direct call");
    assert_eq!(a.develop_json(), b.develop_json(), "{id}: develop state differs from the direct call");
}

const PROFILE: &str = "hsk-lens-profile 1\nname Test 35\nsample 35 - inf\nperspective 1 1 0.5 0.5 -0.08 0 0\nvignette -0.3 0 0\nca_red_green 1.003 0 0 0\n";
const PRESET: &str = "hsk-develop-preset 1\nmeta name Punch\nset contrast 35\nset effects.grain.size 60\n";

fn invert_cube() -> String {
    let mut s = String::from("TITLE \"negative\"\nLUT_3D_SIZE 3\n");
    for b in 0..3 {
        for g in 0..3 {
            for r in 0..3 {
                s.push_str(&format!("{} {} {}\n", 1.0 - r as f64 / 2.0, 1.0 - g as f64 / 2.0, 1.0 - b as f64 / 2.0));
            }
        }
    }
    s
}

struct Files {
    profile: PathBuf,
    cube: PathBuf,
    preset: PathBuf,
}

fn files(scratch: &Scratch) -> Files {
    let f = Files { profile: scratch.0.join("lens.txt"), cube: scratch.0.join("negative.cube"), preset: scratch.0.join("punch.preset") };
    std::fs::write(&f.profile, PROFILE).unwrap();
    std::fs::write(&f.cube, invert_cube()).unwrap();
    std::fs::write(&f.preset, PRESET).unwrap();
    f
}

fn text(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

/// Every row over an existing DevelopOp / FamilyOp member and the two slider rows equal the direct call.
#[test]
fn every_existing_develop_op_has_a_typed_row_equal_to_the_direct_call() {
    let scratch = Scratch::new("develop-commands-existing");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(160, 120)).unwrap();
    let f = files(&scratch);
    let (mut a, mut b) = (open(&source), open(&source));

    same(&mut a, &mut b, "photo.develop.set", json!({"control":"develop.exposure","value":0.5}), |v| v.set_develop("develop.exposure", 0.5));
    same(&mut a, &mut b, "photo.develop.set_many", json!({"changes":{"develop.contrast":20,"develop.shadows":30}}), |v| v.set_develop_many(&[("develop.contrast", 20.0), ("develop.shadows", 30.0)]));
    same(&mut a, &mut b, "photo.develop.add_colour_range", json!({"samples":[[0.5,0.4,0.3]],"breadth":0.3}), |v| v.develop_op(DevelopOp::AddColourRange { samples: vec![[0.5, 0.4, 0.3]], breadth: 0.3 }));
    same(&mut a, &mut b, "photo.develop.add_correction", json!({"source":"linear"}), |v| v.develop_op(DevelopOp::AddCorrection(default_linear())));
    let radial = RadialGradient { left: 0.2, top: 0.25, right: 0.75, bottom: 0.75, angle: 0.0, midpoint: 50.0, roundness: 0.0, feather: 30.0, flipped: false };
    same(&mut a, &mut b, "photo.develop.add_correction", json!({"source":"radial","left":0.2,"feather":30}), |v| v.develop_op(DevelopOp::AddCorrection(MaskSource::RadialGradient(radial))));
    let brush = Brush { dabs: vec![(0.3, 0.3), (0.4, 0.4)], radius: 0.05, flow: 1.0, feather: 0.5 };
    same(&mut a, &mut b, "photo.develop.add_correction", json!({"source":"brush","dabs":[[0.3,0.3],[0.4,0.4]],"radius":0.05}), |v| v.develop_op(DevelopOp::AddCorrection(MaskSource::Brush(brush))));
    same(&mut a, &mut b, "photo.develop.set", json!({"control":"develop.local.3.exposure","value":0.5}), |v| v.set_develop("develop.local.3.exposure", 0.5));
    let lum = MaskSource::LuminanceRange(LuminanceRange { lo: 0.5, hi: 1.0, feather: 0.1 });
    same(&mut a, &mut b, "photo.develop.add_mask", json!({"correction":3,"source":"luminance_range","lo":0.5,"hi":1.0,"feather":0.1,"subtract":true}), |v| v.develop_op(DevelopOp::AddMask { correction: 3, source: lum, subtract: true }));
    let colour = MaskSource::ColourRange(ColourRange { samples: vec![[0.2, 0.3, 0.4]], breadth: 0.5 });
    same(&mut a, &mut b, "photo.develop.add_mask", json!({"correction":1,"source":"colour_range","samples":[[0.2,0.3,0.4]],"breadth":0.5}), |v| v.develop_op(DevelopOp::AddMask { correction: 1, source: colour, subtract: false }));
    let lens = FamilyOp::LensProfile { profile: PROFILE.as_bytes().to_vec(), focal_mm: 35.0, aperture_value: None, distance_m: None };
    same(&mut a, &mut b, "photo.develop.lens_profile", json!({"path":text(&f.profile),"focal_mm":35}), |v| v.develop_op(DevelopOp::Family(lens)));
    // Both toggles are on by default (develop `Optics::default`): turn lateral CA removal off.
    same(&mut a, &mut b, "photo.develop.lens_toggles", json!({"profile_enable":true,"auto_ca":false}), |v| v.develop_op(DevelopOp::Family(FamilyOp::LensToggles { profile_enable: true, auto_ca: false })));
    same(&mut a, &mut b, "photo.develop.upright_guides", json!({"guides":[[[0.1,0.1],[0.12,0.9]],[[0.9,0.1],[0.88,0.9]]]}), |v| {
        v.develop_op(DevelopOp::Family(FamilyOp::UprightGuides(vec![[[0.1, 0.1], [0.12, 0.9]], [[0.9, 0.1], [0.88, 0.9]]])))
    });
    same(&mut a, &mut b, "photo.develop.upright", json!({"mode":"level"}), |v| v.develop_op(DevelopOp::Family(FamilyOp::Upright(UprightMode::Level))));
    same(&mut a, &mut b, "photo.develop.set", json!({"control":"develop.effects.grain.amount","value":40}), |v| v.set_develop("develop.effects.grain.amount", 40.0));
    same(&mut a, &mut b, "photo.develop.grain_seed", json!({"seed":12345}), |v| v.develop_op(DevelopOp::Family(FamilyOp::GrainSeed(12_345))));
    same(&mut a, &mut b, "photo.develop.vignette_style", json!({"style":"colour_priority"}), |v| v.develop_op(DevelopOp::Family(FamilyOp::VignetteStyle(VignetteStyle::ColourPriority))));
    let look = FamilyOp::Look { name: "negative".into(), cube: invert_cube().into_bytes(), domain: LookDomain::SrgbEncoded };
    same(&mut a, &mut b, "photo.develop.look", json!({"path":text(&f.cube),"name":"negative","domain":"srgb_encoded"}), |v| v.develop_op(DevelopOp::Family(look)));
    same(&mut a, &mut b, "photo.develop.apply_preset", json!({"path":text(&f.preset),"amount":0.8}), |v| v.develop_op(DevelopOp::Family(FamilyOp::Preset { preset: PRESET.as_bytes().to_vec(), amount: 0.8 })));
    same(&mut a, &mut b, "photo.develop.clear_look", json!({}), |v| v.develop_op(DevelopOp::Family(FamilyOp::ClearLook)));
    same(&mut a, &mut b, "photo.develop.clear_lens_profile", json!({}), |v| v.develop_op(DevelopOp::Family(FamilyOp::ClearLensProfile)));
    assert!(a.develop_json()["local"].as_array().is_some_and(|l| l.len() == 4), "{}", a.develop_json());
}

/// A file parameter is read under a named cap before parsing; a token, a count or a number out of shape
/// is a typed refusal before anything runs, and nothing is committed.
#[test]
fn develop_rows_refuse_hostile_parameters_typed_and_commit_nothing() {
    let scratch = Scratch::new("develop-commands-hostile");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(64, 48)).unwrap();
    let f = files(&scratch);
    let mut v = open(&source);
    let (pixels, steps) = (digests(&v), undo_len(&v));
    // A .cube one byte over Prism's cap: refused on the size, no byte read (a sparse length, no write).
    let huge = scratch.0.join("huge.cube");
    std::fs::File::create(&huge).unwrap().set_len(hsk_studio_prism::MAX_CUBE_BYTES as u64 + 1).unwrap();
    let preset_over = scratch.0.join("over.preset");
    std::fs::File::create(&preset_over).unwrap().set_len(hsk_studio_develop::MAX_PRESET_BYTES as u64 + 1).unwrap();
    let lens_over = scratch.0.join("over.lens");
    std::fs::File::create(&lens_over).unwrap().set_len(hsk_studio_develop::MAX_LENS_PROFILE_BYTES as u64 + 1).unwrap();
    let unknown_key = scratch.0.join("unknown.preset");
    std::fs::write(&unknown_key, "hsk-develop-preset 1\nset no.such.key 1\n").unwrap();
    let missing = text(&scratch.0.join("absent.cube"));
    let many_guides: Vec<Value> = (0..100_000).map(|_| json!([[0.1, 0.1], [0.1, 0.9]])).collect();
    let cases: Vec<(&str, Value, &str)> = vec![
        ("photo.develop.look", json!({"path":text(&huge),"name":"x","domain":"linear"}), "resource_too_large"),
        ("photo.develop.look", json!({"path":missing,"name":"x","domain":"linear"}), "resource_unreadable"),
        ("photo.develop.look", json!({"path":text(&f.cube),"name":"x","domain":"log"}), "parameter_invalid"),
        ("photo.develop.apply_preset", json!({"path":text(&preset_over),"amount":1}), "resource_too_large"),
        ("photo.develop.apply_preset", json!({"path":text(&unknown_key),"amount":1}), "invalid_preset"),
        ("photo.develop.lens_profile", json!({"path":text(&lens_over),"focal_mm":35}), "resource_too_large"),
        ("photo.develop.lens_profile", json!({"path":text(&f.profile),"focal_mm":"NaN"}), "parameter_invalid"),
        ("photo.develop.upright_guides", json!({"guides":many_guides}), "develop_limit"),
        ("photo.develop.upright", json!({"mode":"sideways"}), "parameter_invalid"),
        ("photo.develop.vignette_style", json!({"style":"loud"}), "parameter_invalid"),
        ("photo.develop.grain_seed", json!({"seed":-1}), "parameter_invalid"),
        ("photo.develop.set", json!({"control":"develop.exposure","value":"inf"}), "parameter_invalid"),
        ("photo.develop.set", json!({"control":"develop.no_such","value":1}), "unknown_control"),
        ("photo.develop.set_many", json!({"changes":{}}), "nothing_to_change"),
        ("photo.develop.set_many", json!({"changes":{"develop.exposure":"x"}}), "parameter_invalid"),
        ("photo.develop.add_correction", json!({"source":"radial","zero":[0.1,0.1]}), "parameter_invalid"),
        ("photo.develop.add_correction", json!({"source":"wormhole"}), "parameter_invalid"),
        ("photo.develop.add_correction", json!({"source":"subject"}), "model_unavailable"),
        ("photo.develop.add_mask", json!({"correction":9,"source":"linear"}), "unknown_item"),
        ("photo.develop.add_colour_range", json!({"samples":[[0.1,0.2]],"breadth":0.3}), "parameter_invalid"),
    ];
    for (id, params, code) in cases {
        let refused = row(&mut v, id, params).expect_err(id);
        assert_eq!(refused.code, code, "{id}: {refused:?}");
    }
    assert_eq!((digests(&v), undo_len(&v)), (pixels, steps), "a refused row commits nothing");
}

// ---- the develop must-haves (item 3, 4) ----------------------------------------------------------

fn ok_row(v: &mut Viewer, id: &str, params: Value) -> Value {
    row(v, id, params).unwrap_or_else(|r| panic!("{id} refused: {r:?}"))
}

/// Group enables are operator state: an override suppresses a family without discarding it, and
/// clearing it returns to the derived value. Point curves switch the tone curve on. Each is one undo
/// step, saved and reopened.
#[test]
fn group_enables_and_point_curves_are_state_one_undo_step_each_and_reopen() {
    let scratch = Scratch::new("develop-commands-enables");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(160, 120)).unwrap();
    let mut v = open(&source);
    ok_row(&mut v, "photo.develop.set", json!({"control":"develop.calibration.red_hue","value":40}));
    let on = digests(&v);
    assert!(v.develop_recipe().unwrap().enables.enable_calibration);
    let steps = undo_len(&v);
    ok_row(&mut v, "photo.develop.set_enable", json!({"group":"calibration","on":false}));
    assert_eq!(undo_len(&v), steps + 1, "set_enable is one undo step");
    let r = v.develop_recipe().unwrap();
    assert!(!r.enables.enable_calibration && r.adjust.calibration.red_hue == 40.0, "the group is off and keeps its parameters");
    assert_ne!(digests(&v), on, "the override suppresses the group");
    assert_eq!(v.develop_json()["enables_override"], json!({"calibration": false}));
    ok_row(&mut v, "photo.develop.clear_enable", json!({"group":"calibration"}));
    assert_eq!(digests(&v), on, "cleared: derived again, the same pixels");
    assert_eq!(v.develop_json()["enables_override"], json!({}));
    let refused = row(&mut v, "photo.develop.set_enable", json!({"group":"red_eye","on":true})).unwrap_err();
    assert_eq!(refused.code, "unsupported_contribution", "red-eye stays refused: {refused:?}");

    let flat = digests(&v);
    ok_row(&mut v, "photo.develop.set_point_curve", json!({"channel":"composite","points":[[0,0],[64,40],[192,215],[255,255]]}));
    ok_row(&mut v, "photo.develop.set_point_curve", json!({"channel":"red","points":[[0,0],[128,150],[255,255]]}));
    assert_ne!(digests(&v), flat, "a point curve moves pixels");
    let r = v.develop_recipe().unwrap();
    assert!(r.enables.enable_tone_curve);
    assert_eq!(r.curves.red.points(), &[(0.0, 0.0), (128.0, 150.0), (255.0, 255.0)]);
    ok_row(&mut v, "photo.develop.set_enable", json!({"group":"tone_curve","on":false}));
    assert_eq!(digests(&v), flat, "tone curve group off: the curves stay, their contribution goes");
    assert_eq!(v.develop_json()["point_curves"]["red"], json!([[0.0, 0.0], [128.0, 150.0], [255.0, 255.0]]));
    for (points, what) in [(json!([[0,0],[128,100],[100,120],[255,255]]), "unsorted x"), (json!([[0,0],[128,100],[128,120],[255,255]]), "duplicate x"), (json!([[0,10],[255,255]]), "endpoint")] {
        let refused = row(&mut v, "photo.develop.set_point_curve", json!({"channel":"green","points":points})).unwrap_err();
        assert_eq!(refused.code, "invalid_curve", "{what}: {refused:?}");
    }

    let state = v.develop_json();
    let (pixels, recipe) = (digests(&v), v.develop_recipe());
    v.undo().unwrap();
    assert!(v.develop_recipe().unwrap().enables.enable_tone_curve, "undo restores the derived tone curve enable");
    v.redo().unwrap();
    let saved = scratch.0.join("enables.handshake");
    v.save(&saved, false).unwrap();
    let w = open(&saved);
    assert_eq!(w.develop_json()["entry"], "Current");
    assert_eq!((digests(&w), w.develop_recipe()), (pixels, recipe), "the override and the curves reopen");
    assert_eq!(w.develop_json()["enables_override"], state["enables_override"]);
}

/// Crop and straighten are stored, applied by the develop export and named in the state; the canvas
/// stays the uncropped develop (`crop_preview_not_applied`), so later slider edits keep working.
#[test]
fn crop_is_stored_applied_at_export_and_the_preview_stays_uncropped() {
    let scratch = Scratch::new("develop-commands-crop");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(160, 120)).unwrap();
    let mut v = open(&source);
    let uncropped = digests(&v);
    let full = scratch.0.join("full.png");
    let report = v.export_develop(&full, DevelopExport::Png { depth: OutputDepth::Eight }, false).unwrap();
    let (fw, fh) = (report["width"].as_u64().unwrap(), report["height"].as_u64().unwrap());
    ok_row(&mut v, "photo.develop.set_crop", json!({"left":0.1,"top":0.2,"right":0.9,"bottom":0.8,"angle":3.5,"flip_horizontal":true}));
    assert_eq!(digests(&v), uncropped, "the canvas is not cropped");
    let state = v.develop_json();
    assert_eq!(state["crop_preview"], "crop_preview_not_applied", "{state}");
    assert_eq!(state["crop"]["angle"], 3.5);
    ok_row(&mut v, "photo.develop.set", json!({"control":"develop.exposure","value":0.3}));
    let out = scratch.0.join("cropped.png");
    let report = v.export_develop(&out, DevelopExport::Png { depth: OutputDepth::Eight }, false).unwrap();
    let (w, h) = (report["width"].as_u64().unwrap(), report["height"].as_u64().unwrap());
    assert!(w < fw && h < fh, "the export is cropped: {w}x{h} of {fw}x{fh}");
    let r = v.develop_recipe().unwrap();
    assert_eq!((r.crop.left, r.crop.angle, r.crop.flip_horizontal, r.crop.flip_vertical), (0.1, 3.5, true, false));
    for (params, code) in [
        (json!({"left":0.8,"top":0.0,"right":0.2,"bottom":1.0}), "empty_crop"),
        (json!({"left":0.0,"top":0.0,"right":1.0,"bottom":1.0,"angle":60}), "invalid_adjustment"),
        (json!({"left":"NaN","top":0.0,"right":1.0,"bottom":1.0}), "parameter_invalid"),
    ] {
        let refused = row(&mut v, "photo.develop.set_crop", params).unwrap_err();
        assert_eq!(refused.code, code, "{refused:?}");
    }
    let saved = scratch.0.join("crop.handshake");
    v.save(&saved, false).unwrap();
    let w2 = open(&saved);
    assert_eq!(w2.develop_recipe().map(|r| r.crop), Some(r.crop), "the crop reopens");
}

/// Snapshots are complete named states (lens binding and point curves included), saved, applied and
/// deleted as one undo step each; a preset is written no-clobber and applies on another frame.
#[test]
fn snapshots_and_preset_save_round_trip() {
    let scratch = Scratch::new("develop-commands-snapshots");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(160, 120)).unwrap();
    let f = files(&scratch);
    let mut v = open(&source);
    ok_row(&mut v, "photo.develop.set_many", json!({"changes":{"develop.contrast":25,"develop.vibrance":30}}));
    ok_row(&mut v, "photo.develop.lens_profile", json!({"path":text(&f.profile),"focal_mm":35}));
    ok_row(&mut v, "photo.develop.set_point_curve", json!({"channel":"blue","points":[[0,0],[128,110],[255,255]]}));
    let (kept, recipe) = (digests(&v), v.develop_recipe());
    let steps = undo_len(&v);
    ok_row(&mut v, "photo.develop.snapshot.save", json!({"name":"Warm v1"}));
    assert_eq!(undo_len(&v), steps + 1, "snapshot.save is one undo step");
    assert_eq!(digests(&v), kept, "a snapshot moves no pixel");
    assert_eq!(row(&mut v, "photo.develop.snapshot.save", json!({"name":"Warm v1"})).unwrap_err().code, "develop_snapshot_exists");
    assert_eq!(row(&mut v, "photo.develop.snapshot.save", json!({"name":" padded"})).unwrap_err().code, "develop_name_invalid");
    ok_row(&mut v, "photo.develop.clear_lens_profile", json!({}));
    ok_row(&mut v, "photo.develop.set_point_curve", json!({"channel":"blue","points":[[0,0],[255,255]]}));
    ok_row(&mut v, "photo.develop.set", json!({"control":"develop.contrast","value":-50}));
    assert_ne!(digests(&v), kept);
    ok_row(&mut v, "photo.develop.snapshot.apply", json!({"name":"Warm v1"}));
    assert_eq!((digests(&v), v.develop_recipe()), (kept.clone(), recipe.clone()), "the snapshot restores the whole state, profile and curves too");
    assert_eq!(v.develop_json()["snapshots"], json!(["Warm v1"]));
    assert_eq!(row(&mut v, "photo.develop.snapshot.apply", json!({"name":"absent"})).unwrap_err().code, "unknown_item");

    // Preset save: an output row, no-clobber, a typed refusal for unknown keys.
    let preset = scratch.0.join("look.preset");
    let saved = ok_row(&mut v, "photo.develop.save_preset", json!({"path":text(&preset),"name":"Warm","keys":["contrast","vibrance"]}));
    assert_eq!(saved["keys"], 2, "{saved}");
    let bytes = std::fs::read(&preset).unwrap();
    assert_eq!(row(&mut v, "photo.develop.save_preset", json!({"path":text(&preset),"name":"Warm","keys":["contrast"]})).unwrap_err().code, "destination_exists");
    assert_eq!(std::fs::read(&preset).unwrap(), bytes, "no clobber");
    let other = scratch.0.join("other.preset");
    assert_eq!(row(&mut v, "photo.develop.save_preset", json!({"path":text(&other),"name":"Bad","keys":["no.such.key"]})).unwrap_err().code, "invalid_preset");
    assert!(!other.exists());
    assert_eq!(row(&mut v, "photo.develop.save_preset", json!({"path":text(&source),"name":"Bad","keys":["contrast"],"overwrite":true})).unwrap_err().code, "export_over_source");
    // Scenario step 51: the saved preset on another frame at 80 percent.
    let mut o = open(&source);
    ok_row(&mut o, "photo.develop.apply_preset", json!({"path":text(&preset),"amount":0.8}));
    let r = o.develop_recipe().unwrap();
    assert_eq!((r.adjust.tone.contrast, r.adjust.presence.vibrance), (20.0, 24.0));

    ok_row(&mut v, "photo.develop.snapshot.save", json!({"name":"Second"}));
    let saved_pkg = scratch.0.join("snap.handshake");
    v.save(&saved_pkg, false).unwrap();
    let mut w = open(&saved_pkg);
    assert_eq!(w.develop_json()["snapshots"], json!(["Warm v1", "Second"]));
    ok_row(&mut w, "photo.develop.snapshot.delete", json!({"name":"Warm v1"}));
    assert_eq!(w.develop_json()["snapshots"], json!(["Second"]));
    w.undo().unwrap();
    assert_eq!(w.develop_json()["snapshots"], json!(["Warm v1", "Second"]), "undo brings a deleted snapshot back");
}

/// Entries without the new members keep their bytes; an entry with them round-trips; the hostile
/// decoder bounds every new member before decoding it.
#[test]
fn develop_entry_new_members_are_absent_by_default_and_bounded() {
    let scratch = Scratch::new("develop-commands-entry");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(64, 48)).unwrap();
    let mut v = open(&source);
    ok_row(&mut v, "photo.develop.set", json!({"control":"develop.contrast","value":10}));
    let layer = v.develop_json()["layer_id"].as_str().unwrap().to_string();
    let plain = develop_entry::encode(&layer, None, &v.develop_edit().unwrap()).unwrap();
    let state = v.run().document_entry("develop").unwrap().to_vec();
    let value: Value = serde_json::from_slice(&state).unwrap();
    for key in ["enables_override", "point_curves", "crop", "snapshots"] {
        assert!(value["edit"].get(key).is_none(), "{key} absent when default");
    }
    assert_eq!(develop_entry::decode(&plain).unwrap().edit, v.develop_edit().unwrap());
    ok_row(&mut v, "photo.develop.set_enable", json!({"group":"detail","on":false}));
    ok_row(&mut v, "photo.develop.set_crop", json!({"left":0.0,"top":0.1,"right":1.0,"bottom":0.9}));
    ok_row(&mut v, "photo.develop.snapshot.save", json!({"name":"one"}));
    let bytes = v.run().document_entry("develop").unwrap().to_vec();
    assert!(develop_entry::decode(&bytes).is_ok());
    let saved = scratch.0.join("entry.handshake");
    v.save(&saved, false).unwrap();
    let w = open(&saved);
    assert_eq!(w.run().document_entry("develop"), Some(bytes.as_slice()), "the entry is stable over save and reopen");

    let hostile = |patch: &dyn Fn(&mut Value), code: &str, what: &str| {
        let mut v: Value = serde_json::from_slice(&bytes).unwrap();
        patch(&mut v);
        let b = serde_json::to_vec(&v).unwrap();
        assert_eq!(develop_entry::decode(&b).map(|_| ()).map_err(|r| r.code), Err(code), "{what}");
    };
    hostile(&|v| v["edit"]["enables_override"] = json!({"no_group": true}), "develop_entry_field", "unknown group");
    hostile(&|v| v["edit"]["enables_override"] = json!({"detail": 1}), "develop_entry_field", "non-boolean override");
    hostile(&|v| v["edit"]["point_curves"] = json!({"red": (0..1000).map(|i| [i, i]).collect::<Vec<_>>()}), "develop_entry_limit", "too many points");
    hostile(&|v| v["edit"]["point_curves"] = json!({"red": [[0,0],[50,60],[40,70],[255,255]]}), "develop_entry_field", "unsorted points");
    hostile(&|v| v["edit"]["crop"]["extra"] = json!(1), "develop_entry_field", "crop key");
    hostile(&|v| v["edit"]["crop"]["left"] = json!("x"), "develop_entry_field", "crop number");
    hostile(&|v| v["edit"]["snapshots"] = json!((0..100_000).map(|_| json!({})).collect::<Vec<_>>()), "develop_entry_limit", "10^5 snapshots");
    hostile(&|v| v["edit"]["snapshots"][0]["name"] = json!("x".repeat(300)), "develop_entry_field", "long name");
    hostile(&|v| { let s = v["edit"]["snapshots"][0].clone(); v["edit"]["snapshots"].as_array_mut().unwrap().push(s); }, "develop_entry_field", "duplicate snapshot name");
    hostile(&|v| v["edit"]["snapshots"][0]["edit"]["snapshots"] = json!([]), "develop_entry_field", "nested snapshots");
}

/// The rows of the new ops equal a direct `develop_op(DevelopOp::Recipe(..))` with the same op.
#[test]
fn every_new_develop_op_row_equals_the_direct_call() {
    let scratch = Scratch::new("develop-commands-new-ops");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(96, 64)).unwrap();
    let (mut a, mut b) = (open(&source), open(&source));
    let recipe = |op: RecipeOp| move |v: &mut Viewer| v.develop_op(DevelopOp::Recipe(op));
    same(&mut a, &mut b, "photo.develop.set_enable", json!({"group":"detail","on":false}), recipe(RecipeOp::SetEnable { group: "detail".into(), on: Some(false) }));
    same(&mut a, &mut b, "photo.develop.clear_enable", json!({"group":"detail"}), recipe(RecipeOp::SetEnable { group: "detail".into(), on: None }));
    let points = vec![(0.0, 0.0), (100.0, 80.0), (255.0, 255.0)];
    same(&mut a, &mut b, "photo.develop.set_point_curve", json!({"channel":"green","points":[[0,0],[100,80],[255,255]]}), recipe(RecipeOp::SetPointCurve { channel: CurveChannel::Green, points }));
    let crop = NormalizedCrop { left: 0.1, top: 0.0, right: 0.9, bottom: 1.0, angle: -2.0, flip_vertical: true, ..NormalizedCrop::full() };
    same(&mut a, &mut b, "photo.develop.set_crop", json!({"left":0.1,"top":0,"right":0.9,"bottom":1,"angle":-2,"flip_vertical":true}), recipe(RecipeOp::SetCrop(crop)));
    same(&mut a, &mut b, "photo.develop.snapshot.save", json!({"name":"A"}), recipe(RecipeOp::SaveSnapshot("A".into())));
    same(&mut a, &mut b, "photo.develop.set", json!({"control":"develop.contrast","value":40}), |v| v.set_develop("develop.contrast", 40.0));
    same(&mut a, &mut b, "photo.develop.snapshot.apply", json!({"name":"A"}), recipe(RecipeOp::ApplySnapshot("A".into())));
    same(&mut a, &mut b, "photo.develop.snapshot.delete", json!({"name":"A"}), recipe(RecipeOp::DeleteSnapshot("A".into())));
    assert_eq!(a.develop_extras(), b.develop_extras());
}

/// R60 attempt-2 probe on SMB-COMMAND-SURFACE-1 (`mutating-row-declared-non-mutating`): every
/// `photo.info.*` row writes the session entry `info` as one undo step (samplers are persistent
/// document state, STU-RAS-163 and STU-COL-232), so its row declares it mutates; a row that makes no
/// step declares it does not.
#[test]
fn photo_info_rows_declare_the_undo_step_they_make() {
    let scratch = Scratch::new("develop-commands-info");
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(16, 16, "paper", [200, 100, 50])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    for (id, params) in [
        ("photo.info.add_sampler", json!({"x":2,"y":3})),
        ("photo.info.move_sampler", json!({"index":0,"x":4,"y":5})),
        ("photo.info.set_sampler_model", json!({"index":0,"model":"lab"})),
        ("photo.info.set_sample_size", json!({"size":"s5"})),
        ("photo.info.set_sample_scope", json!({"scope":"current_layer"})),
        ("photo.info.set_model", json!({"model":"hsb"})),
        ("photo.info.remove_sampler", json!({"index":0})),
    ] {
        let steps = undo_len(&v);
        row(&mut v, id, params).unwrap_or_else(|r| panic!("{id}: {r:?}"));
        let stepped = undo_len(&v) > steps;
        let declared = action::command(id).unwrap_or_else(|| panic!("{id} row")).mutates_authority;
        assert_eq!(declared, stepped, "{id}: mutates_authority {declared}, undo step made {stepped}");
    }
}

// ---- process test --------------------------------------------------------------------------------

const PROCESS_LIMIT: Duration = Duration::from_secs(300);

#[cfg(feature = "desktop")]
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_studio-harness"))
}

#[cfg(not(feature = "desktop"))]
fn binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().and_then(Path::parent).expect("target directory").to_path_buf();
    let example = dir.join("examples").join(format!("studio-harness{}", std::env::consts::EXE_SUFFIX));
    assert!(example.is_file(), "the studio-harness example was not built at {}", example.display());
    example
}

struct Live {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: std::io::BufReader<std::process::ChildStdout>,
}

impl Live {
    fn open(source: &Path, diag: &Path) -> Self {
        let args = ["command", source.to_str().unwrap(), "--actor", "lane-develop", "--session", "s-develop", "--stdin"];
        let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        assert!(!matches!(cli::window_args(&owned, &|p| p.is_file()), WindowArgs::Window { .. }), "{args:?} would open a window; not spawned");
        let mut command = Command::new(binary());
        command.args(args).current_dir(diag).env("STUDIO_DIAG_DIR", diag).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().expect("spawn studio-harness");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = std::io::BufReader::new(child.stdout.take().expect("stdout"));
        let mut live = Self { child, stdin, stdout };
        let opened = live.read();
        assert_eq!(opened["ok"], true, "{opened}");
        live
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        std::io::BufRead::read_line(&mut self.stdout, &mut line).expect("a reply line");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("reply is not JSON ({e}): {line}"))
    }

    fn ok(&mut self, id: &str, parameters: Value) -> Value {
        writeln!(self.stdin, "{}", json!({"command":"command","command_id":id,"parameters":parameters})).unwrap();
        self.stdin.flush().unwrap();
        let reply = self.read();
        assert_eq!(reply["ok"], true, "{id}: {} {}", reply["error"], reply["detail"]);
        reply
    }

    /// One step: ok, one named undo step.
    fn step(&mut self, id: &str, parameters: Value) -> Value {
        let reply = self.ok(id, parameters);
        assert!(reply["detail"]["undo_name"].is_string(), "{id} names its undo step: {reply}");
        reply
    }

    fn close(mut self) -> i32 {
        drop(self.stdin);
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                return status.code().expect("an exit code, not a crash");
            }
            if start.elapsed() > PROCESS_LIMIT {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("the session did not end within {PROCESS_LIMIT:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Scenario photo-retouch steps 27 (look), 28 (lens profile), 29 (CA and defringe), 30 (Upright), 40
/// (radial dodge), 41 (brush with a luminance range mask; intersect is not a develop blend, so the range
/// is added), 46 (vignette and grain) and 51 (preset at 80 percent), in order, through the process.
#[test]
fn photo_retouch_develop_steps_run_through_the_process() {
    let scratch = Scratch::new("develop-commands-process");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(96, 64)).unwrap();
    let f = files(&scratch);
    let mut s = Live::open(&source, &scratch.0);
    s.step("photo.develop.look", json!({"path":text(&f.cube),"name":"negative","domain":"srgb_encoded"}));
    s.step("photo.develop.set", json!({"control":"develop.look.amount","value":0.6}));
    s.step("photo.develop.lens_profile", json!({"path":text(&f.profile),"focal_mm":35}));
    // Profile correction and lateral CA removal are on by default: the row confirms them, no step.
    let toggles = s.ok("photo.develop.lens_toggles", json!({"profile_enable":true,"auto_ca":true}));
    assert!(toggles["detail"]["undo_name"].is_null(), "{toggles}");
    s.step("photo.develop.set_many", json!({"changes":{"develop.optics.defringe.purple_amount":5,"develop.optics.defringe.green_amount":3}}));
    s.step("photo.develop.upright", json!({"mode":"level"}));
    s.step("photo.develop.add_correction", json!({"source":"radial","left":0.3,"top":0.2,"right":0.7,"bottom":0.6}));
    s.step("photo.develop.set", json!({"control":"develop.local.0.exposure","value":0.4}));
    s.step("photo.develop.add_correction", json!({"source":"brush","dabs":[[0.4,0.35],[0.6,0.35]],"radius":0.04}));
    s.step("photo.develop.add_mask", json!({"correction":1,"source":"luminance_range","lo":0.4,"hi":1.0}));
    s.step("photo.develop.set_many", json!({"changes":{"develop.effects.vignette.amount":-40,"develop.effects.grain.amount":25}}));
    s.step("photo.develop.grain_seed", json!({"seed":7}));
    let last = s.step("photo.develop.apply_preset", json!({"path":text(&f.preset),"amount":0.8}));
    assert_eq!(last["state"]["develop"]["local"].as_array().map(Vec::len), Some(2), "{last}");
    assert_eq!(s.close(), 0);
}
