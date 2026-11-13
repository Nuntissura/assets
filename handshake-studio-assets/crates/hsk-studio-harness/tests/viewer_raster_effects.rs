//! Lane SMB-PS-FILTER-ADJUST-COMMANDS-1 (feature `viewer`): the STU-FX-007 effect commands over a raster layer's smart
//! filter stack (`effect.*`), the STU-RAS-103..112 parameter specs a model plans with (`effect.parameters`), the
//! STU-RAS-035 adjustment layer and its destructive apply-in-place form (`adjustment.*`), and the develop crop in the
//! DNG XMP (STU-RAW-015, through the lrcat codec). Each mutating row is one named undo step, survives save and
//! reopen, and reaches the frame and the export; refusals are typed and change nothing. One process test drives the
//! rows through `command --stdin`. Headless (no window, CX-SAFE-002). Photoshop and Lightroom parity UNVERIFIED (D5).
#![cfg(feature = "viewer")]

mod common;

use common::{L, Scratch, layered_psd, synthetic_dng};
use hsk_studio_harness::cli::{self, WindowArgs};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::commands::CommandRefusal;
use hsk_studio_harness::view::develop::DevelopExport;
use hsk_studio_harness::view::export::ExportFormat;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_interop_tiff as tiff;
use serde_json::{Map, Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// 32x32: `bg` opaque (40, 80, 120); `art` a 16x16 square at (8, 8) whose red channel alternates 0 / 255 per
/// pixel (hard edges a blur changes), green 128, blue 64.
fn art_psd() -> Vec<u8> {
    let n = 32 * 32;
    let red: Vec<u8> = (0..256).map(|i| if (i + i / 16) % 2 == 0 { 0 } else { 255 }).collect();
    layered_psd(
        32,
        32,
        &[
            L { name: "bg", rect: [0, 0, 32, 32], channels: vec![(0, vec![40; n]), (1, vec![80; n]), (2, vec![120; n])] },
            L { name: "art", rect: [8, 8, 24, 24], channels: vec![(0, red), (1, vec![128; 256]), (2, vec![64; 256])] },
        ],
    )
}

/// One opaque 16x16 layer `solo` with a ramp in every channel (no pixel at 0 or 1).
fn solo_psd() -> Vec<u8> {
    let ramp: Vec<u8> = (0..256u32).map(|i| (20 + (i * 7) % 200) as u8).collect();
    let green: Vec<u8> = ramp.iter().rev().copied().collect();
    layered_psd(16, 16, &[L { name: "solo", rect: [0, 0, 16, 16], channels: vec![(0, ramp.clone()), (1, green), (2, vec![90; 256])] }])
}

fn open(path: &Path) -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    v
}

fn layer(v: &Viewer, name: &str) -> String {
    let snap = v.run().snapshot().unwrap();
    snap.document().layers.iter().find(|l| l.name == name).unwrap_or_else(|| panic!("layer {name}")).layer_id.clone()
}

fn undo(v: &Viewer) -> Vec<String> {
    v.run().history()["undo"].as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect()
}

fn row(v: &mut Viewer, id: &str, params: Value) -> Result<Value, CommandRefusal> {
    let map: Map<String, Value> = params.as_object().cloned().unwrap_or_default();
    v.run_command(id, &map)
}

/// The row succeeds as ONE undo step named by its id (mutating rows), and returns its value.
fn ok(v: &mut Viewer, id: &str, params: Value) -> Value {
    let before = undo(v);
    let out = row(v, id, params).unwrap_or_else(|r| panic!("{id} refused: {} {}", r.code, r.detail));
    let after = undo(v);
    if !matches!(id, "effect.read" | "effect.parameters") {
        assert_eq!(after.len(), before.len() + 1, "{id} is one undo step: {after:?}");
        assert!(after.last().unwrap().starts_with(id), "{id} names its step: {after:?}");
    } else {
        assert_eq!(after, before, "{id} reads only");
    }
    out
}

/// The row refuses with `code` and changes neither the history nor the raster records entry.
fn refused(v: &mut Viewer, id: &str, params: Value, code: &str) -> CommandRefusal {
    let (steps, entry) = (undo(v), v.run().document_entry("raster_records").map(<[u8]>::to_vec));
    let r = row(v, id, params.clone()).expect_err(&format!("{id} {params} should refuse {code}"));
    assert_eq!(r.code, code, "{id} {params}: {}", r.detail);
    assert_eq!(undo(v), steps, "{id}: a refusal makes no step");
    assert_eq!(v.run().document_entry("raster_records").map(<[u8]>::to_vec), entry, "{id}: a refusal leaves the entry");
    r
}

fn frame(v: &mut Viewer) -> Vec<u8> {
    v.render_if_dirty();
    v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json()["status"])).rgba8.clone()
}

fn filters(v: &mut Viewer, layer: &str) -> Value {
    ok(v, "effect.read", json!({"layer": layer}))
}

#[test]
fn effect_rows_edit_the_stack_one_step_each_and_reach_frame_save_and_export() {
    let scratch = Scratch::new("raster-effects-stack");
    let src = scratch.0.join("art.psd");
    std::fs::write(&src, art_psd()).unwrap();
    let mut v = open(&src);
    let art = layer(&v, "art");
    let plain = frame(&mut v);
    assert_eq!(filters(&mut v, &art)["filters"], json!([]));

    let a = ok(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "gaussian_blur", "params": {"radius_px": 2}}));
    assert_eq!((a["instance_id"].as_str(), a["index"].as_u64()), (Some("fx-1"), Some(0)), "{a}");
    let blurred = frame(&mut v);
    assert_ne!(blurred, plain, "the smart filter renders");
    // Declared defaults fill the omitted parameters (Fast Box Blur table: radius 0, iterations 3, H and V).
    let b = ok(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "box_blur", "opacity": 50}));
    assert_eq!(b["defaulted"], json!(["radius_px", "iterations", "dimensions"]), "{b}");
    let read = filters(&mut v, &art);
    assert_eq!(read["filters"][1]["params"], json!({"radius_px": 0.0, "iterations": 3, "dimensions": "horizontal_and_vertical"}), "{read}");
    assert_eq!(read["filters"][1]["opacity"], 50.0);

    ok(&mut v, "effect.update", json!({"layer": art, "instance": "fx-1", "params": {"radius_px": 3.5}}));
    assert_eq!(filters(&mut v, &art)["filters"][0]["params"]["radius_px"], 3.5);
    assert_ne!(frame(&mut v), blurred, "the update re-renders");
    ok(&mut v, "effect.reorder", json!({"layer": art, "instance": "fx-2", "to": 0}));
    let order: Vec<Value> = filters(&mut v, &art)["filters"].as_array().unwrap().iter().map(|f| f["instance_id"].clone()).collect();
    assert_eq!(order, vec![json!("fx-2"), json!("fx-1")]);
    ok(&mut v, "effect.toggle", json!({"layer": art, "instance": "fx-1", "enabled": false}));
    assert_eq!(filters(&mut v, &art)["filters"][1]["enabled"], false);
    ok(&mut v, "effect.toggle", json!({"layer": art, "enabled": false}));
    assert_eq!(frame(&mut v), plain, "the master switch off renders the layer unfiltered");
    ok(&mut v, "effect.toggle", json!({"layer": art, "enabled": true}));
    ok(&mut v, "effect.toggle", json!({"layer": art, "instance": "fx-1", "enabled": true}));
    let both = frame(&mut v);
    ok(&mut v, "effect.remove", json!({"layer": art, "instance": "fx-2"}));
    assert_eq!(filters(&mut v, &art)["filters"].as_array().unwrap().len(), 1);
    // Undo restores the removed entry; redo removes it again.
    v.undo().unwrap();
    assert_eq!(filters(&mut v, &art)["filters"].as_array().unwrap().len(), 2);
    assert_eq!(frame(&mut v), both);
    v.redo().unwrap();
    let kept = filters(&mut v, &art);
    let shown = frame(&mut v);

    // Save, reopen: the same stack and the same frame.
    let saved = scratch.0.join("effects.handshake");
    v.save(&saved, false).unwrap();
    let mut w = open(&saved);
    assert_eq!(filters(&mut w, &art), kept);
    assert_eq!(frame(&mut w), shown);
    // Export: the filtered document exports other pixels than the plain source, and the reopened one the same.
    let (png_v, png_w, png_plain) = (scratch.0.join("v.png"), scratch.0.join("w.png"), scratch.0.join("plain.png"));
    v.export(&png_v, ExportFormat::Png, false).unwrap();
    w.export(&png_w, ExportFormat::Png, false).unwrap();
    open(&src).export(&png_plain, ExportFormat::Png, false).unwrap();
    let bytes = |p: &PathBuf| std::fs::read(p).unwrap();
    assert_eq!(bytes(&png_v), bytes(&png_w), "the reopened document exports the same pixels");
    assert_ne!(bytes(&png_v), bytes(&png_plain), "the export carries the smart filter");
    // Removing the last entry removes the stack (no stack mask): the record set is as before the first apply.
    ok(&mut v, "effect.remove", json!({"layer": art, "instance": "fx-1"}));
    assert_eq!(filters(&mut v, &art)["enabled"], Value::Null);
    assert_eq!(frame(&mut v), plain);
}

#[test]
fn parameter_specs_are_model_visible_and_out_of_range_values_refuse_naming_the_bounds() {
    let scratch = Scratch::new("raster-effects-params");
    let src = scratch.0.join("art.psd");
    std::fs::write(&src, art_psd()).unwrap();
    let mut v = open(&src);
    let (art, bg) = (layer(&v, "art"), layer(&v, "bg"));

    // STU-RAS-110: every kind's specs without running a command; STU-RAS-111: the contract check passes them.
    let all = ok(&mut v, "effect.parameters", json!({}));
    assert_eq!(all["check"], "STUDIO_EFFECT_PARAM_CONTRACT");
    assert_eq!(all["findings"], json!([]), "{}", all["findings"]);
    assert_eq!(all["kinds"].as_array().unwrap().len(), 25);
    let spec = |kind: &str, key: &str| -> Value {
        let k = all["kinds"].as_array().unwrap().iter().find(|k| k["filter_kind"] == kind).unwrap_or_else(|| panic!("{kind}"));
        k["parameters"].as_array().unwrap().iter().find(|p| p["param_key"] == key).unwrap_or_else(|| panic!("{kind}.{key}")).clone()
    };
    let radius = spec("box_blur", "radius_px");
    // STU-RAS-104: hard and soft are separate values; here the table declares both and they differ.
    assert_eq!([&radius["hard_min"], &radius["hard_max"], &radius["soft_min"], &radius["soft_max"]], [&json!(0.0), &json!(30000.0), &json!(0.0), &json!(127.0)]);
    assert_eq!(radius["bound_classification"]["soft_max"], "declared");
    assert_eq!([&radius["step"], &radius["coarse_step"], &radius["fine_step"]], [&json!(0.1), &json!(1.0), &json!(0.1)], "STU-RAS-108 from precision 1");
    // STU-RAS-106: what no source declared is `unknown`, never a copy of the other pair; STU-RAS-107: no observation.
    let gauss = spec("gaussian_blur", "radius_px");
    assert_eq!((gauss["soft_min"].as_str(), gauss["default"].as_str(), gauss["precision"].as_str()), (Some("unknown"), Some("unknown"), Some("unknown")));
    assert_eq!((gauss["bound_classification"]["hard_max"].as_str(), gauss["observed_max"].as_str()), (Some("derived"), Some("absent")));
    let angle = spec("motion_blur", "angle_deg");
    assert_eq!((angle["hard_min"].as_str(), angle["hard_max"].as_str()), (Some("unknown"), Some("unknown")));
    assert_eq!(spec("radial_blur", "kind")["options"], json!([{"token": "spin", "discriminant": 1}, {"token": "zoom", "discriminant": 2}]));
    let one = ok(&mut v, "effect.parameters", json!({"filter_kind": "mosaic"}));
    assert_eq!(one["kinds"].as_array().unwrap().len(), 1);
    refused(&mut v, "effect.parameters", json!({"filter_kind": "melt"}), "parameter_invalid");

    // The STU-RAS-111 check on altered specs (each defect class): harness-view unit test
    // `the_contract_check_finds_each_defect_class`.

    // STU-FX-104: outside the hard range is a determinate refusal naming the parameter, value and both bounds.
    let r = refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "box_blur", "params": {"radius_px": 30001}}), "effect_param_out_of_range");
    assert_eq!(r.detail, json!({"filter_kind": "box_blur", "param": "radius_px", "value": 30001, "hard_min": 0.0, "hard_max": 30000.0}));
    let r = refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "surface_blur", "params": {"radius_px": 2, "threshold_levels": 0}}), "effect_param_out_of_range");
    assert_eq!(r.detail["hard_min_exclusive"], true);
    let zeros = vec![0; 25];
    let r = refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "custom_convolution", "params": {"weights": zeros, "scale": 0, "offset": 0}}), "effect_param_out_of_range");
    assert_eq!(r.detail["excludes"], json!([0]));
    refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "solarize", "params": {"threshold_levels": 255}}), "effect_param_out_of_range");
    // STU-FX-105: beyond the soft range inside the hard one is legal, and the read shows the extended state.
    ok(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "box_blur", "params": {"radius_px": 500, "iterations": 1}}));
    assert_eq!(filters(&mut v, &art)["filters"][0]["out_of_soft_range"], json!(["radius_px"]));
    // Typed refusals before anything runs.
    refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "gaussian_blur"}), "parameter_missing");
    refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "gaussian_blur", "params": {"radius_px": "wide"}}), "parameter_invalid");
    refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "gaussian_blur", "params": {"radius_px": 2, "sigma": 1}}), "parameter_unknown");
    refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "median", "params": {"radius_px": 1.5}}), "parameter_invalid");
    refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "melt"}), "parameter_invalid");
    refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "add_noise", "params": {"amount_pct": 5, "distribution": "pink", "monochromatic": true, "seed": 1}}), "parameter_invalid");
    refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "sharpen", "opacity": 101}), "parameter_out_of_range");
    refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "sharpen", "at": 9}), "index_out_of_range");
    refused(&mut v, "effect.apply", json!({"layer": "SLYR-019abcde-0000-7000-8000-00000000ffff", "filter_kind": "sharpen"}), "layer_unknown");
    refused(&mut v, "effect.update", json!({"layer": art, "instance": "fx-9", "params": {"radius_px": 1}}), "effect_instance_unknown");
    refused(&mut v, "effect.update", json!({"layer": art, "instance": "fx-1", "params": {"radius_px": -1}}), "effect_param_out_of_range");
    refused(&mut v, "effect.update", json!({"layer": art, "instance": "fx-1", "params": {"radius_px": 500}}), "nothing_to_change");
    refused(&mut v, "effect.reorder", json!({"layer": art, "instance": "fx-1", "to": 4}), "index_out_of_range");
    refused(&mut v, "effect.toggle", json!({"layer": art, "instance": "fx-1", "enabled": true}), "nothing_to_change");
    refused(&mut v, "effect.toggle", json!({"layer": bg, "enabled": false}), "effect_instance_unknown");
    refused(&mut v, "effect.remove", json!({"layer": bg, "instance": "fx-1"}), "effect_instance_unknown");
    // The stack holds at most Folio's 16 entries.
    for _ in 1..16 {
        ok(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "sharpen"}));
    }
    refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "sharpen"}), "effect_stack_full");
    // An adjustment layer is not a pixel layer.
    let adj = ok(&mut v, "adjustment.create", json!({"adjustment": {"kind": "invert"}}))["layer_id"].as_str().unwrap().to_string();
    refused(&mut v, "effect.apply", json!({"layer": adj, "filter_kind": "sharpen"}), "layer_not_pixel");
}

#[test]
fn adjustment_layers_are_created_edited_and_applied_in_place_like_the_layer_renders() {
    let scratch = Scratch::new("raster-effects-adjust");
    let src = scratch.0.join("solo.psd");
    std::fs::write(&src, solo_psd()).unwrap();
    let mut v = open(&src);
    let solo = layer(&v, "solo");
    let plain = frame(&mut v);

    let created = ok(&mut v, "adjustment.create", json!({"adjustment": {"kind": "invert"}, "name": "Invert 1"}));
    let adj = created["layer_id"].as_str().unwrap().to_string();
    assert!(adj.contains("-7ede-9300-"), "{adj}");
    assert_eq!(created["kind"], "invert");
    let inverted = frame(&mut v);
    assert_ne!(inverted, plain);
    {
        let snap = v.run().snapshot().unwrap();
        let l = snap.document().layers.iter().find(|l| l.layer_id == adj).unwrap();
        assert_eq!((l.kind.as_str(), l.name.as_str()), ("adjustment", "Invert 1"));
    }
    // The destructive form on a second viewer of the same source gives the same frame as the layer (STU-RAS-035).
    let mut w = open(&src);
    let applied = ok(&mut w, "adjustment.apply", json!({"layer": solo, "adjustment": {"kind": "invert"}}));
    assert_eq!(applied["pixels"], 256, "{applied}");
    let close = |a: &[u8], b: &[u8]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.abs_diff(*y) <= 1);
    assert!(close(&frame(&mut w), &inverted), "apply in place equals the adjustment layer");
    let levels = json!({"kind": "levels", "records": [{"channel": "composite", "input_black": 0.1, "input_white": 0.9, "output_black": 0.0, "output_white": 1.0, "gamma": 1.4}]});
    ok(&mut v, "adjustment.edit", json!({"layer": adj, "adjustment": levels}));
    let leveled = frame(&mut v);
    w.undo().unwrap();
    assert_eq!(frame(&mut w), plain, "undo restores the pixels");
    ok(&mut w, "adjustment.apply", json!({"layer": solo, "adjustment": levels}));
    assert!(close(&frame(&mut w), &leveled), "levels in place equals the levels layer");
    refused(&mut v, "adjustment.edit", json!({"layer": adj, "adjustment": levels}), "nothing_to_change");

    // A clipped adjustment layer; save and reopen keep both layers' records and the frame.
    // Inserted above `solo` (the selected layer): a layer clipped to an adjustment layer is refused by Folio.
    v.select(Some(solo.clone()));
    let clipped = ok(&mut v, "adjustment.create", json!({"adjustment": {"kind": "exposure", "exposure": 0.5, "offset": 0.0, "gamma": 1.0, "use_legacy": false}, "clipped": true}));
    assert_eq!(clipped["clipped"], true);
    let shown = frame(&mut v);
    let saved = scratch.0.join("adjust.handshake");
    v.save(&saved, false).unwrap();
    let mut r = open(&saved);
    assert_eq!(frame(&mut r), shown, "the reopened adjustment layers render the same");
    {
        let snap = r.run().snapshot().unwrap();
        let l = snap.document().layers.iter().find(|l| l.layer_id == clipped["layer_id"].as_str().unwrap()).unwrap();
        assert!(l.appearance.as_ref().is_some_and(|a| a.clipped), "clipped survives reopen");
    }
    ok(&mut r, "adjustment.edit", json!({"layer": adj, "adjustment": {"kind": "invert"}}));

    // Typed refusals that change nothing.
    refused(&mut v, "adjustment.apply", json!({"layer": adj, "adjustment": {"kind": "invert"}}), "layer_not_pixel");
    refused(&mut v, "adjustment.edit", json!({"layer": solo, "adjustment": {"kind": "invert"}}), "layer_not_adjustment");
    let r = refused(&mut v, "adjustment.create", json!({"adjustment": {"kind": "vibrance", "vibrance": 150, "saturation": 0}}), "adjust_invalid_parameter");
    assert_eq!(r.detail["param"], "vibrance.vibrance");
    refused(&mut v, "adjustment.create", json!({"adjustment": {"kind": "ocio"}}), "parameter_invalid");
    refused(&mut v, "adjustment.create", json!({"adjustment": {"kind": "invert"}, "name": ""}), "parameter_invalid");
    refused(&mut v, "adjustment.apply", json!({"layer": solo, "adjustment": "not a record"}), "parameter_invalid");
}

/// The develop crop reaches the DNG's XMP as crs: keys written by the lrcat codec; without a crop the caller's
/// packet is embedded byte for byte, as before.
#[test]
fn the_develop_dng_xmp_carries_the_crop_keys() {
    use hsk_studio_develop::{DngCompression, DngPreview};
    let scratch = Scratch::new("raster-effects-dng-crop");
    let source = scratch.0.join("synthetic.dng");
    std::fs::write(&source, synthetic_dng(64, 48)).unwrap();
    let dng = DevelopExport::Dng { compression: DngCompression::None, preview: DngPreview::Medium, allow_dropped: true };
    let xmp_of = |path: &Path| -> String {
        let bytes = std::fs::read(path).unwrap();
        let c = tiff::Container::parse(&bytes, &tiff::Limits::default()).unwrap();
        let ifds = c.walk().unwrap();
        let entry = ifds[0].find(700).expect("XMP tag 700");
        String::from_utf8(c.bytes(entry).unwrap().to_vec()).unwrap()
    };
    // A crs: value written as an attribute or as an element.
    let has = |xmp: &str, key: &str, value: &str| xmp.contains(&format!("crs:{key}=\"{value}\"")) || xmp.contains(&format!("<crs:{key}>{value}</crs:{key}>"));
    let caller = "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:keep=\"urn:example:keep\" keep:Note=\"untouched\"/></rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>";

    let mut plain = Viewer::new(Prefer::Cpu);
    plain.open(&source).unwrap();
    plain.set_develop_dng_xmp(Some(caller.as_bytes().to_vec())).unwrap();
    let out = scratch.0.join("plain.dng");
    plain.export_develop(&out, dng, false).unwrap();
    assert_eq!(xmp_of(&out), caller, "no crop: the caller's packet byte for byte");

    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    row(&mut v, "photo.develop.set_crop", json!({"left": 0.25, "top": 0.2, "right": 0.75, "bottom": 0.8, "angle": 3})).unwrap();
    let fresh = scratch.0.join("fresh.dng");
    let report = v.export_develop(&fresh, dng, false).unwrap();
    assert_eq!(report["develop_crop"]["xmp"], "crs_crop_keys", "{report}");
    assert_eq!(report["develop_crop"]["notes"], json!([]), "{report}");
    let xmp = xmp_of(&fresh);
    for (key, value) in [("CropLeft", "0.250000"), ("CropTop", "0.200000"), ("CropRight", "0.750000"), ("CropBottom", "0.800000"), ("CropAngle", "+3.00"), ("HasCrop", "True")] {
        assert!(has(&xmp, key, value), "{key}={value} in {xmp}");
    }
    // Into the caller's packet: its own property stays, the crop keys join it.
    v.set_develop_dng_xmp(Some(caller.as_bytes().to_vec())).unwrap();
    let merged = scratch.0.join("merged.dng");
    v.export_develop(&merged, dng, false).unwrap();
    let xmp = xmp_of(&merged);
    assert!(xmp.contains("keep:Note=\"untouched\"") && has(&xmp, "CropLeft", "0.250000") && has(&xmp, "HasCrop", "True"), "{xmp}");
    // A packet the codec cannot read refuses typed and writes nothing.
    v.set_develop_dng_xmp(Some(b"<x:xmpmeta><rdf:RDF><<</x:xmpmeta>".to_vec())).unwrap();
    let never = scratch.0.join("never.dng");
    let e = v.export_develop(&never, dng, false).unwrap_err();
    assert_eq!(e.code, "xmp_invalid", "{}", e.detail);
    assert!(!never.exists());
}

/// Lead ruling 2026-11-13 (layers.rs mint_layer_id): a layer created after a delete never takes the deleted layer's
/// id, so it never inherits the deleted layer's `raster_records` row (its smart filter stack) the entry still holds;
/// in the session and after save and reopen.
#[test]
fn a_new_layer_after_a_delete_gets_a_fresh_id_and_no_inherited_smart_filters() {
    let scratch = Scratch::new("raster-effects-reused-id");
    let src = scratch.0.join("art.psd");
    std::fs::write(&src, art_psd()).unwrap();
    let mut v = open(&src);
    let gone = v.new_raster_layer("filtered").unwrap();
    ok(&mut v, "effect.apply", json!({"layer": gone, "filter_kind": "gaussian_blur", "params": {"radius_px": 2}}));
    v.delete_layer(&gone).unwrap();
    let fresh = v.new_raster_layer("fresh").unwrap();
    assert_ne!(fresh, gone, "a deleted layer's id is not minted again");
    assert_eq!(filters(&mut v, &fresh)["filters"], json!([]), "the new layer inherits no smart filter stack");

    // The deleted layer's row is still in the saved entry; a layer created after reopening gets another id too.
    let mut w = open(&src);
    let gone = w.new_raster_layer("filtered").unwrap();
    ok(&mut w, "effect.apply", json!({"layer": gone, "filter_kind": "box_blur"}));
    w.delete_layer(&gone).unwrap();
    let saved = scratch.0.join("deleted.handshake");
    w.save(&saved, false).unwrap();
    let mut r = open(&saved);
    let fresh = r.new_raster_layer("fresh").unwrap();
    assert_ne!(fresh, gone, "a deleted layer's id is not minted again after reopen");
    assert_eq!(filters(&mut r, &fresh)["filters"], json!([]), "the new layer inherits no smart filter stack after reopen");
}

// ---- the rows through the process (`command --stdin`) -----------------------------------------------------------

const FIXTURE: &[u8] = include_bytes!("fixtures/demo.handshake");
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";
const PROCESS_LIMIT: Duration = Duration::from_secs(120);

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
    fn open(package: &Path, actor: &str, diag: &Path) -> Self {
        let args = ["command", package.to_str().unwrap(), "--actor", actor, "--session", "s-effects", "--stdin"];
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

    fn call(&mut self, id: &str, parameters: Value) -> Value {
        writeln!(self.stdin, "{}", json!({"command":"command","command_id":id,"parameters":parameters})).unwrap();
        self.stdin.flush().unwrap();
        self.read()
    }

    fn ok(&mut self, id: &str, parameters: Value) -> Value {
        let reply = self.call(id, parameters);
        assert_eq!(reply["ok"], true, "{id}: {} {}", reply["error"], reply["detail"]);
        reply
    }

    fn close(mut self) -> i32 {
        drop(self.stdin);
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                return status.code().expect("an exit code, not a crash");
            }
            assert!(start.elapsed() < PROCESS_LIMIT, "the session did not end");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn effect_and_adjustment_rows_run_through_the_process_with_the_callers_attribution() {
    let dir = Scratch::new("raster-effects-process");
    let package = dir.0.join("a.handshake");
    std::fs::write(&package, FIXTURE).unwrap();
    let mut s = Live::open(&package, "lane-fx", &dir.0);
    let applied = s.ok("effect.apply", json!({"layer": L2, "filter_kind": "gaussian_blur", "params": {"radius_px": 1.5}, "opacity": 80}));
    assert!(applied["detail"]["undo_name"].as_str().is_some_and(|n| n.starts_with("effect.apply")), "{applied}");
    assert_eq!(applied["attribution"]["actor"]["account_id"], "lane-fx", "{applied}");
    assert_eq!(applied["detail"]["result"]["instance_id"], "fx-1");
    let read = s.ok("effect.read", json!({"layer": L2}));
    assert_eq!(read["detail"]["result"]["filters"][0]["opacity"], 80.0, "{read}");
    assert!(read["detail"]["undo_name"].is_null());
    let specs = s.ok("effect.parameters", json!({"filter_kind": "box_blur"}));
    assert_eq!(specs["detail"]["result"]["kinds"][0]["parameters"][0]["hard_max"], 30000.0);
    let refused = s.call("effect.update", json!({"layer": L2, "instance": "fx-1", "params": {"radius_px": 1001}}));
    assert_eq!((refused["ok"].as_bool(), refused["error"].as_str()), (Some(false), Some("effect_param_out_of_range")), "{refused}");
    assert_eq!(refused["detail"]["param"], "radius_px", "{refused}");
    let created = s.ok("adjustment.create", json!({"adjustment": {"kind": "invert"}}));
    assert!(created["detail"]["undo_name"].as_str().is_some_and(|n| n.starts_with("adjustment.create")), "{created}");
    assert_eq!(created["attribution"]["actor"]["account_id"], "lane-fx");
    assert_eq!(s.close(), 0);
}
