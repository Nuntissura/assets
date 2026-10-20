//! Lane SMB-DEVELOP-LENS-PRESETS harness slice (feature `viewer`): lens corrections, transform /
//! Upright, effects, calibration, creative looks, presets and model-mask buttons in the develop panel
//! and the stored `develop` entry; every change is one undo step; hostile entries and values are
//! typed refusals. Headless (no window, CX-SAFE-002). Lightroom parity UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, synthetic_dng};
use hsk_studio_develop::{LookDomain, ModelMask, ModelMaskKind, UprightMode, VignetteStyle};
use hsk_studio_folio::Payload;
use hsk_studio_harness::view::develop::{DevelopOp, Refusal};
use hsk_studio_harness::view::develop_entry;
use hsk_studio_harness::view::develop_more::FamilyOp;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::Viewer;
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

fn ok(r: Result<u64, Refusal>) -> u64 {
    r.unwrap_or_else(|e| panic!("refused: {e}"))
}

fn undo_len(v: &Viewer) -> usize {
    v.run().history()["undo"].as_array().unwrap().len()
}

const PROFILE: &str = "hsk-lens-profile 1\nname Test 35\nsample 35 - inf\nperspective 1 1 0.5 0.5 -0.08 0 0\nvignette -0.3 0 0\nca_red_green 1.003 0 0 0\n";

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

#[test]
fn lens_transform_effects_calibration_look_and_preset_develop_undo_and_reopen() {
    let scratch = Scratch::new("develop-lens-families");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(160, 120)).unwrap();
    let mut v = open(&source);
    let mut last = digests(&v);
    let mut steps = undo_len(&v);
    macro_rules! moves {
        ($what:expr, $call:expr) => {{
            ok($call);
            let now = digests(&v);
            assert_ne!(now, last, "{} changed no pixel", $what);
            last = now;
            steps += 1;
            assert_eq!(undo_len(&v), steps, "{} is one undo step", $what);
        }};
    }
    moves!("manual distortion + lens vignette", v.set_develop_many(&[("develop.optics.manual_distortion", 40.0), ("develop.optics.manual_vignette", 30.0)]));
    moves!("lens profile", v.develop_op(DevelopOp::Family(FamilyOp::LensProfile { profile: PROFILE.as_bytes().to_vec(), focal_mm: 35.0, aperture_value: None, distance_m: None })));
    moves!("perspective", v.set_develop_many(&[("develop.geometry.vertical", 25.0), ("develop.geometry.rotate", 2.5)]));
    moves!("calibration", v.set_develop_many(&[("develop.calibration.red_hue", 40.0), ("develop.calibration.shadow_tint", -30.0)]));
    moves!("post-crop vignette", v.set_develop("develop.effects.vignette.amount", -60.0));
    moves!("vignette style", v.develop_op(DevelopOp::Family(FamilyOp::VignetteStyle(VignetteStyle::ColourPriority))));
    moves!("grain", v.set_develop("develop.effects.grain.amount", 40.0));
    moves!("grain seed", v.develop_op(DevelopOp::Family(FamilyOp::GrainSeed(12_345))));
    moves!("look", v.develop_op(DevelopOp::Family(FamilyOp::Look { name: "negative".into(), cube: invert_cube().into_bytes(), domain: LookDomain::SrgbEncoded })));
    moves!("look amount", v.set_develop("develop.look.amount", 0.4));
    moves!("preset", v.develop_op(DevelopOp::Family(FamilyOp::Preset { preset: b"hsk-develop-preset 1\nmeta name Punch\nset contrast 35\nset effects.grain.size 60\n".to_vec(), amount: 1.0 })));
    ok(v.develop_op(DevelopOp::Family(FamilyOp::Upright(UprightMode::Level))));
    steps += 1;
    assert_eq!(undo_len(&v), steps, "upright mode is one undo step");
    let state = v.develop_json();
    let f = &state["families"];
    assert_eq!((f["upright"].as_str(), f["grain_seed"].as_u64(), f["vignette_style"].as_str()), (Some("level"), Some(12_345), Some("colour_priority")), "{state}");
    assert_eq!(f["lens_profile"]["name"], "Test 35");
    assert_eq!(f["look"]["size_3d"], 3);
    let recipe = v.develop_recipe().unwrap();
    let e = recipe.enables;
    assert!(e.enable_lens_corrections && e.enable_transform && e.enable_effects && e.enable_calibration, "{state}");
    assert_eq!(recipe.adjust.tone.contrast, 35.0);

    // The panel draws the new ids and commits nothing.
    let ops = v.run().ops().len();
    let ctx = egui::Context::default();
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 2400.0))), ..Default::default() };
    let _ = ctx.run(input, |ctx| v.ui(ctx));
    for name in [
        "develop.optics.manual_distortion", "develop.optics.defringe.purple_amount", "develop.geometry.vertical", "develop.effects.grain.amount",
        "develop.effects.vignette.feather", "develop.calibration.blue_saturation", "develop.look.amount", "develop.upright.vertical",
        "develop.lens.clear", "develop.lens.auto_ca", "develop.grain.reseed", "develop.vignette.style", "develop.look.clear",
        "develop.local.add_model.subject", "develop.local.add_model.sky",
    ] {
        assert!(v.drawn().iter().any(|d| d == name), "{name} not drawn");
    }
    assert_eq!(v.run().ops().len(), ops);

    // Undo walks back one family step; save and reopen restore the whole edit (lens binding, look).
    let before_undo = v.develop_edit().unwrap();
    v.undo().unwrap();
    assert_eq!(v.develop_edit().unwrap().adjust.geometry.upright_mode, UprightMode::Off);
    v.redo().unwrap();
    assert_eq!(v.develop_edit(), Some(before_undo.clone()));
    let saved = scratch.0.join("saved.handshake");
    v.save(&saved, false).unwrap();
    let w = open(&saved);
    assert_eq!(w.develop_json()["entry"], "Current");
    assert_eq!(w.develop_edit(), Some(before_undo));
    assert_eq!(w.develop_recipe(), Some(recipe));
}

#[test]
fn model_masks_are_refused_and_commit_nothing() {
    let scratch = Scratch::new("develop-lens-model");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(64, 48)).unwrap();
    let mut v = open(&source);
    let (pixels, undo, edit) = (digests(&v), undo_len(&v), v.develop_edit());
    for kind in ModelMaskKind::ALL {
        let r = v.develop_op(DevelopOp::AddCorrection(hsk_studio_develop::MaskSource::Model(ModelMask { kind, subcategory: Some(50_008), model_version: None })));
        let refusal = r.unwrap_err();
        assert_eq!(refusal.code, "model_unavailable", "{kind:?}");
        assert!(refusal.detail.contains(kind.key()), "{}", refusal.detail);
    }
    assert_eq!((digests(&v), undo_len(&v), v.develop_edit()), (pixels, undo, edit));
}

#[test]
fn hostile_family_entries_and_values_are_typed_refusals() {
    let scratch = Scratch::new("develop-lens-hostile");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(64, 48)).unwrap();
    let mut v = open(&source);
    ok(v.develop_op(DevelopOp::Family(FamilyOp::LensProfile { profile: PROFILE.as_bytes().to_vec(), focal_mm: 35.0, aperture_value: Some(4.0), distance_m: Some(f64::INFINITY) })));
    ok(v.develop_op(DevelopOp::Family(FamilyOp::Look { name: "negative".into(), cube: invert_cube().into_bytes(), domain: LookDomain::Linear })));
    ok(v.develop_op(DevelopOp::Family(FamilyOp::UprightGuides(vec![[[0.2, 0.1], [0.25, 0.9]], [[0.8, 0.1], [0.75, 0.9]]]))));
    let layer = v.develop_json()["layer_id"].as_str().unwrap().to_string();
    let good = develop_entry::encode(&layer, None, &v.develop_edit().unwrap()).unwrap();
    assert_eq!(develop_entry::decode(&good).map(|s| s.edit).ok(), v.develop_edit(), "lossless with a lens binding, a look and guides");
    let base: Value = serde_json::from_slice(&good).unwrap();
    let mutate = |f: &dyn Fn(&mut Value)| {
        let mut x = base.clone();
        f(&mut x);
        serde_json::to_vec(&x).unwrap()
    };
    // An entry written before this lane (no family keys) decodes with identity families.
    let old = mutate(&|x| {
        for k in ["optics", "geometry", "effects", "calibration", "look"] {
            x["edit"].as_object_mut().unwrap().remove(k);
        }
    });
    let decoded = develop_entry::decode(&old).unwrap().edit;
    assert!(decoded.adjust.look.is_none() && decoded.adjust.optics.profile.is_none());
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("defringe short", mutate(&|x| x["edit"]["optics"]["defringe"] = json!([0, 30, 70, 0, 40]))),
        ("upright mode unknown", mutate(&|x| x["edit"]["geometry"]["upright_mode"] = json!("sideways"))),
        ("five candidates", mutate(&|x| x["edit"]["geometry"]["upright_transforms"] = json!(vec![vec![1.0; 9]; 5]))),
        ("five guides", mutate(&|x| x["edit"]["geometry"]["upright_guides"] = json!(vec![[[0.1, 0.1], [0.1, 0.9]]; 5]))),
        ("seed over u32", mutate(&|x| x["edit"]["effects"]["grain_seed"] = json!(1u64 << 33))),
        ("vignette style unknown", mutate(&|x| x["edit"]["effects"]["vignette_style"] = json!("sepia"))),
        ("calibration short", mutate(&|x| x["edit"]["calibration"] = json!([0, 0, 0]))),
        ("look cube garbage", mutate(&|x| x["edit"]["look"]["cube"] = json!("LUT_3D_SIZE 9000\n"))),
        ("look sha short", mutate(&|x| x["edit"]["look"]["sha256"] = json!("abcd"))),
        ("binding perspective short", mutate(&|x| x["edit"]["optics"]["profile"]["perspective"] = json!([1, 1, 0.5]))),
        ("binding distance text", mutate(&|x| x["edit"]["optics"]["profile"]["distance_m"] = json!("far"))),
        ("unknown family key", mutate(&|x| x["edit"]["optics2"] = json!({}))),
    ];
    for (what, bytes) in &cases {
        assert_eq!(develop_entry::decode(bytes).map(|_| ()).map_err(|r| r.code), Err("develop_entry_field"), "{what}");
    }

    let (pixels, undo, edit) = (digests(&v), undo_len(&v), v.develop_edit());
    let values: [(&str, f64, &str); 5] = [
        ("develop.geometry.rotate", 20.0, "invalid_adjustment"),
        ("develop.optics.distortion_scale", 250.0, "invalid_adjustment"),
        ("develop.effects.grain.amount", f64::NAN, "non_finite"),
        ("develop.calibration.green_hue", 150.0, "invalid_adjustment"),
        ("develop.look.amount", 2.5, "invalid_adjustment"),
    ];
    for (id, value, code) in values {
        assert_eq!(v.set_develop(id, value).map_err(|r| r.code), Err(code), "{id} = {value}");
    }
    let family_refusals: [(FamilyOp, &str); 4] = [
        (FamilyOp::LensProfile { profile: b"hsk-lens-profile 1\nname x\n".to_vec(), focal_mm: 35.0, aperture_value: None, distance_m: None }, "invalid_lens_profile"),
        (FamilyOp::Look { name: "bad".into(), cube: b"LUT_3D_SIZE 2\n0 0 0\n".to_vec(), domain: LookDomain::Linear }, "invalid_lut"),
        (FamilyOp::Preset { preset: b"hsk-develop-preset 1\nset contrast loud\n".to_vec(), amount: 1.0 }, "invalid_preset"),
        (FamilyOp::Preset { preset: b"hsk-develop-preset 1\nset contrast 50\n".to_vec(), amount: 9.0 }, "invalid_adjustment"),
    ];
    for (op, code) in family_refusals {
        assert_eq!(v.develop_op(DevelopOp::Family(op)).map_err(|r| r.code), Err(code));
    }
    assert_eq!((digests(&v), undo_len(&v), v.develop_edit()), (pixels, undo, edit), "nothing committed");
}
