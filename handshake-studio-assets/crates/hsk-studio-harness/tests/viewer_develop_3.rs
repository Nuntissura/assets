//! SMB-HARNESS-DEVELOP-3 (feature `viewer`): HSL mixer, parametric curve, grayscale mix and linear /
//! radial gradient and brush masks in the develop panel and the stored `develop` entry (develop
//! recipe types only); a structure change is its own undo step even when no pixel moves; hostile
//! entries and values of the new families are typed refusals. Headless (no window, CX-SAFE-002).
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, synthetic_dng};
use hsk_studio_develop::DevelopError;
use hsk_studio_develop::local::MAX_DABS;
use hsk_studio_folio::Payload;
use hsk_studio_harness::view::develop::{DevelopOp, Refusal, default_brush, default_linear, default_radial};
use hsk_studio_harness::view::develop_entry;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::{Viewer, widget};
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

fn revision(v: &Viewer) -> u64 {
    v.run().snapshot().unwrap().document().revision
}

/// Each new family moves pixels through its develop type, the panel draws its ids, and the stored
/// entry brings the whole edit back on reopen.
#[test]
fn mixer_curve_grayscale_gradient_and_brush_masks_develop_and_reopen() {
    let scratch = Scratch::new("develop3-families");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(300, 260)).unwrap();
    let mut v = open(&source);
    let mut last = digests(&v);
    macro_rules! moves {
        ($what:expr, $call:expr) => {{
            ok($call);
            let now = digests(&v);
            assert_ne!(now, last, "{} changed no pixel", $what);
            last = now;
        }};
    }
    moves!("mixer", v.set_develop_many(&[("develop.mixer.red.hue", 40.0), ("develop.mixer.blue.saturation", -50.0), ("develop.mixer.green.luminance", 30.0)]));
    moves!("parametric curve", v.set_develop_many(&[("develop.parametric.shadows", 60.0), ("develop.parametric.highlights", -40.0)]));
    ok(v.develop_op(DevelopOp::AddCorrection(default_linear())));
    moves!("linear gradient", v.set_develop("develop.local.0.exposure", 0.7));
    moves!("linear gradient geometry", v.set_develop_many(&[("develop.local.0.mask.0.zero_y", 0.1), ("develop.local.0.mask.0.full_y", 0.9)]));
    moves!("flipped gradient", v.develop_op(DevelopOp::MaskFlags { correction: 0, mask: 0, inverted: false, subtract: false, flipped: true }));
    ok(v.develop_op(DevelopOp::AddCorrection(default_radial())));
    moves!("radial gradient", v.set_develop("develop.local.1.saturation", -0.8));
    moves!("radial roundness", v.set_develop("develop.local.1.mask.0.roundness", 60.0));
    ok(v.develop_op(DevelopOp::AddCorrection(default_brush())));
    moves!("brush", v.set_develop("develop.local.2.exposure", -0.6));
    moves!("brush dabs", v.develop_op(DevelopOp::AddBrushDabs { correction: 2, mask: 0, dabs: vec![(0.2, 0.2), (0.25, 0.22), (0.3, 0.24)] }));
    moves!("subtracted mask", v.develop_op(DevelopOp::AddMask { correction: 0, source: default_radial(), subtract: true }));
    moves!("grayscale convert", v.develop_op(DevelopOp::Grayscale(true)));
    moves!("grayscale band", v.set_develop("develop.grayscale.red", 70.0));
    assert!(!last.is_empty());
    let state = v.develop_json();
    assert_eq!(state["local"].as_array().map(|l| l.iter().map(|c| c["masks"].clone()).collect::<Vec<_>>()), Some(vec![json!(["linear_gradient", "radial_gradient"]), json!(["radial_gradient"]), json!(["brush"])]));
    let recipe = v.develop_recipe().unwrap();
    let e = recipe.enables;
    assert!(e.enable_colour_adjustments && e.enable_tone_curve && e.enable_grayscale_mix && e.enable_mask_group_based_corrections, "{state}");

    // The panel draws every new family with stable ids and commits nothing.
    let ops = v.run().ops().len();
    let ctx = egui::Context::default();
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 1600.0))), ..Default::default() };
    let _ = ctx.run(input, |ctx| v.ui(ctx));
    for name in [
        "develop.mixer.red.hue", "develop.mixer.magenta.luminance", "develop.parametric.shadow_split", "develop.grayscale.red",
        "develop.local.0.mask.0.zero_x", "develop.local.0.mask.1.roundness", "develop.local.1.mask.0.feather", "develop.local.2.mask.0.radius",
        "develop.local.2.mask.0.flow", "develop.local.0.mask.1.invert", "develop.local.2.mask.0.remove",
        widget::DEVELOP_ADD_LINEAR, widget::DEVELOP_ADD_RADIAL, widget::DEVELOP_ADD_BRUSH, widget::DEVELOP_GRAYSCALE,
    ] {
        assert!(v.drawn().iter().any(|d| d == name), "{name} not drawn");
    }
    assert_eq!(v.run().ops().len(), ops);

    // Save and reopen: the entry restores the whole edit (gradients, brush dabs, mixer, curve, gray).
    let edit = v.develop_edit().unwrap();
    let saved = scratch.0.join("saved.handshake");
    v.save(&saved, false).unwrap();
    let w = open(&saved);
    assert_eq!(w.develop_json()["entry"], "Current");
    assert_eq!(w.develop_edit(), Some(edit));
    assert_eq!(w.develop_recipe(), Some(recipe));
}

/// A new point colour, correction or mask is its own undo step before any value moves: the entry
/// changes (the pixels and the document revision do not), undo removes it, redo brings it back.
#[test]
fn identity_structure_changes_are_their_own_undo_steps() {
    let scratch = Scratch::new("develop3-steps");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(64, 48)).unwrap();
    let mut v = open(&source);
    let (pixels, r0) = (digests(&v), revision(&v));
    for (i, op) in [
        DevelopOp::AddPointColour { source: [0.4, 0.3, 0.2] },
        DevelopOp::AddCorrection(default_linear()),
        DevelopOp::AddMask { correction: 0, source: default_brush(), subtract: false },
    ]
    .into_iter()
    .enumerate()
    {
        ok(v.develop_op(op));
        assert_eq!((undo_len(&v), digests(&v), revision(&v)), (i + 1, pixels.clone(), r0), "step {i}");
        assert_eq!(v.develop_json()["values_known"], true);
    }
    assert_eq!(v.develop_json()["local"][0]["masks"], json!(["linear_gradient", "brush"]));
    v.undo().unwrap();
    assert_eq!(v.develop_json()["local"][0]["masks"], json!(["linear_gradient"]));
    v.undo().unwrap();
    assert_eq!(v.develop_json()["local"], json!([]));
    v.undo().unwrap();
    assert_eq!((v.develop_json()["point_colours"].as_array().map(Vec::len), v.develop_json()["entry"].as_str()), (Some(0), Some("Imported")));
    v.redo().unwrap();
    v.redo().unwrap();
    assert_eq!((v.develop_json()["point_colours"].as_array().map(Vec::len), v.develop_json()["local"].as_array().map(Vec::len)), (Some(1), Some(1)));
    // A value move on top is one more step (pixels change), undone with its entry.
    let edit = v.develop_edit().unwrap();
    ok(v.set_develop("develop.local.0.exposure", 0.5));
    assert_ne!(digests(&v), pixels);
    v.undo().unwrap();
    assert_eq!((digests(&v), v.develop_edit()), (pixels, Some(edit)));
}

/// Hostile entries of the new mask kinds and out-of-range values of the new families: typed
/// refusals that change nothing.
#[test]
fn hostile_new_family_entries_and_values_are_typed_refusals() {
    let scratch = Scratch::new("develop3-hostile");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(64, 48)).unwrap();
    let mut v = open(&source);
    ok(v.develop_op(DevelopOp::AddCorrection(default_brush())));
    ok(v.develop_op(DevelopOp::AddCorrection(default_radial())));
    let layer = v.develop_json()["layer_id"].as_str().unwrap().to_string();
    let good = develop_entry::encode(&layer, None, &v.develop_edit().unwrap()).unwrap();
    assert_eq!(develop_entry::decode(&good).map(|s| s.edit).ok(), v.develop_edit(), "lossless with gradient and brush masks");
    let base: Value = serde_json::from_slice(&good).unwrap();
    let mutate = |f: &dyn Fn(&mut Value)| {
        let mut x = base.clone();
        f(&mut x);
        serde_json::to_vec(&x).unwrap()
    };
    let half = MAX_DABS / 2 + 1;
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("dabs over the cap", mutate(&|x| x["edit"]["local"][0]["masks"][0]["source"]["dabs"] = json!(vec![[0.5, 0.5]; MAX_DABS + 1])), "develop_entry_limit"),
        ("dabs over the cap across brushes", mutate(&|x| {
            x["edit"]["local"][0]["masks"][0]["source"]["dabs"] = json!(vec![[0.5, 0.5]; half]);
            let copy = x["edit"]["local"][0].clone();
            x["edit"]["local"].as_array_mut().unwrap().push(copy);
        }), "develop_entry_limit"),
        ("dab not a pair", mutate(&|x| x["edit"]["local"][0]["masks"][0]["source"]["dabs"] = json!([[0.5, 0.5, 0.5]])), "develop_entry_field"),
        ("dab string", mutate(&|x| x["edit"]["local"][0]["masks"][0]["source"]["dabs"] = json!([["0.5", 0.5]])), "develop_entry_field"),
        ("radial key missing", mutate(&|x| { x["edit"]["local"][1]["masks"][0]["source"].as_object_mut().unwrap().remove("roundness"); }), "develop_entry_field"),
        ("flipped not a bool", mutate(&|x| x["edit"]["local"][1]["masks"][0]["source"]["flipped"] = json!(1)), "develop_entry_field"),
        ("unknown kind", mutate(&|x| x["edit"]["local"][1]["masks"][0]["source"]["kind"] = json!("depth_range")), "develop_entry_field"),
        ("grayscale bands short", mutate(&|x| x["edit"]["grayscale"]["bands"] = json!(vec![0.0; 7])), "develop_entry_field"),
    ];
    for (what, bytes, code) in &cases {
        assert_eq!(develop_entry::decode(bytes).map(|_| ()).map_err(|r| r.code), Err(*code), "{what}");
    }

    let (r0, undo, edit) = (revision(&v), undo_len(&v), v.develop_edit());
    let values: [(&str, f64, &str); 7] = [
        ("develop.mixer.red.hue", 150.0, DevelopError::InvalidAdjustment.code()),
        ("develop.parametric.shadow_split", 90.0, DevelopError::InvalidAdjustment.code()),
        ("develop.grayscale.blue", f64::NAN, DevelopError::NonFinite.code()),
        ("develop.local.0.mask.0.radius", 0.0, DevelopError::InvalidMask.code()),
        ("develop.local.1.mask.0.right", 0.1, DevelopError::InvalidMask.code()),
        ("develop.local.1.mask.0.midpoint", 120.0, DevelopError::InvalidMask.code()),
        ("develop.local.0.mask.0.zero_x", 1.0, "unknown_control"),
    ];
    for (id, value, code) in values {
        assert_eq!(v.set_develop(id, value).map_err(|r| r.code), Err(code), "{id} = {value}");
    }
    let ops: [(DevelopOp, &str); 5] = [
        (DevelopOp::AddCorrection(hsk_studio_develop::MaskSource::LinearGradient(hsk_studio_develop::LinearGradient { zero: (0.5, 0.5), full: (0.5, 0.5), flipped: false })), DevelopError::InvalidMask.code()),
        (DevelopOp::AddBrushDabs { correction: 0, mask: 0, dabs: vec![(40.0, 0.5)] }, DevelopError::InvalidMask.code()),
        (DevelopOp::AddBrushDabs { correction: 1, mask: 0, dabs: vec![(0.5, 0.5)] }, "not_a_brush"),
        (DevelopOp::MaskFlags { correction: 0, mask: 0, inverted: true, subtract: false, flipped: true }, "not_a_gradient"),
        (DevelopOp::RemoveMask { correction: 1, mask: 3 }, "unknown_item"),
    ];
    for (op, code) in ops {
        let label = format!("{op:?}");
        assert_eq!(v.develop_op(op).map_err(|r| r.code), Err(code), "{label}");
    }
    assert_eq!((revision(&v), undo_len(&v), v.develop_edit()), (r0, undo, edit), "refusals change nothing");
}
