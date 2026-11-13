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
use hsk_studio_harness::view::place::{PlaceOptions, PlacementKind};
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
    // STU-RAS-109: every member's integer discriminant, classified (the Radial Blur table declares these).
    assert_eq!(
        spec("radial_blur", "kind")["options"],
        json!([{"token": "spin", "discriminant": 1, "discriminant_classification": "declared"}, {"token": "zoom", "discriminant": 2, "discriminant_classification": "declared"}])
    );
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
    let r = refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "sharpen", "opacity": 101}), "effect_param_out_of_range");
    assert_eq!(r.detail, json!({"param": "opacity", "value": 101.0, "hard_min": 0.0, "hard_max": 100.0}), "review F10: the STU-FX-104 shape");
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
    // Review Q5: one job admitted through the viewer's JobPort, settled with the result.
    assert_eq!(applied["job"]["outcome"], "completed", "{applied}");
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
    let report = plain.export_develop(&out, dng, false).unwrap();
    assert_eq!(xmp_of(&out), caller, "no crop: the caller's packet byte for byte");
    assert_eq!(report["xmp_crop_keys"], "unchanged", "{report}");
    // Review F4: a stale crop in the caller's packet with the Studio crop full is cleared (HasCrop False, the full
    // frame); every other crs: key and the user content stay as the caller wrote them.
    let stale = "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\" xmlns:keep=\"urn:example:keep\" keep:Note=\"untouched\" crs:Exposure2012=\"+1.00\" crs:HasCrop=\"True\" crs:CropLeft=\"0.1\"/></rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>";
    plain.set_develop_dng_xmp(Some(stale.as_bytes().to_vec())).unwrap();
    let cleared = scratch.0.join("cleared.dng");
    let report = plain.export_develop(&cleared, dng, false).unwrap();
    assert_eq!(report["xmp_crop_keys"], "written", "{report}");
    let xmp = xmp_of(&cleared);
    assert!(has(&xmp, "HasCrop", "False") && has(&xmp, "CropLeft", "0.000000"), "{xmp}");
    assert!(xmp.contains("crs:Exposure2012=\"+1.00\"") && xmp.contains("keep:Note=\"untouched\""), "the other keys are the caller's: {xmp}");
    plain.set_develop_dng_xmp(None).unwrap();
    let none = scratch.0.join("none.dng");
    let report = plain.export_develop(&none, dng, false).unwrap();
    assert_eq!((report["xmp_crop_keys"].as_str(), report["xmp_bytes"].as_u64()), (Some("absent"), Some(0)), "no packet, no crop: none: {report}");

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

fn layer_ids(v: &Viewer) -> Vec<String> {
    v.run().snapshot().unwrap().document().layers.iter().map(|l| l.layer_id.clone()).collect()
}

/// The layer ids the `raster_records` entry keys (rows of deleted layers included).
fn record_ids(v: &Viewer) -> Vec<String> {
    let Some(bytes) = v.run().document_entry("raster_records") else { return Vec::new() };
    let entry: Value = serde_json::from_slice(bytes).unwrap();
    entry["records"]["layers"].as_array().map(|a| a.iter().filter_map(|r| r["layer_id"].as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// The 48-bit counter of a minted id (its last group): revision in the upper 24 bits.
fn counter(id: &str) -> u64 {
    u64::from_str_radix(id.rsplit('-').next().unwrap(), 16).unwrap()
}

/// Lead ruling 2026-11-13 on the layer id mint, conditions (2) to (5) through the viewer (conditions (1), (2) at the
/// largest value, (3) and (6) are harness-view unit tests in `view::layers`): one command creating two layers mints
/// two distinct ids of one revision; a package saved at an earlier revision, reopened, mints no id it holds as a
/// layer or as a `raster_records` row; undo then create mints a fresh id; and an id carrying revision bits survives
/// Folio persistence and reaches an export that names it (`export.run` through node settings keyed by it).
#[test]
fn minted_layer_ids_are_never_minted_twice_and_survive_save_reopen_and_export() {
    let scratch = Scratch::new("raster-effects-mint");
    let src = scratch.0.join("art.psd");
    std::fs::write(&src, art_psd()).unwrap();
    let mut v = open(&src);
    let art = layer(&v, "art");
    // (3) Duplicating a group copies the group and `art` in one step: two ids of one revision.
    let group = v.group_layers(std::slice::from_ref(&art), "g").unwrap();
    let before = layer_ids(&v);
    let steps = undo(&v).len();
    let copy = v.duplicate_layer(&group).unwrap();
    assert_eq!(undo(&v).len(), steps + 1, "one step");
    let added: Vec<String> = layer_ids(&v).into_iter().filter(|id| !before.contains(id)).collect();
    assert_eq!(added.len(), 2, "{added:?}");
    assert!(added.contains(&copy) && added[0] != added[1], "{added:?}");
    assert_eq!(counter(&added[0]) >> 24, counter(&added[1]) >> 24, "one revision: {added:?}");
    assert!(counter(&copy) >> 24 > 0, "{copy} carries revision bits");

    // A deleted layer whose raster_records row stays in the entry, and node export settings keyed by the copy.
    let gone = v.new_raster_layer("gone").unwrap();
    ok(&mut v, "effect.apply", json!({"layer": gone, "filter_kind": "sharpen"}));
    v.delete_layer(&gone).unwrap();
    assert!(record_ids(&v).contains(&gone), "the row of the deleted layer stays in the entry");
    let steps = undo(&v).len();
    row(&mut v, "export.set_settings", json!({"node": copy, "settings": [{"format": "PNG"}]})).unwrap();
    assert_eq!(undo(&v).len(), steps + 1);
    let early = scratch.0.join("early.handshake");
    v.save(&early, false).unwrap();
    let saved_ids: Vec<String> = layer_ids(&v).into_iter().chain(record_ids(&v)).collect();

    // (4) Undo, then create: the undone layer's id is not minted again.
    let a = v.new_raster_layer("a").unwrap();
    v.undo().unwrap();
    assert!(!layer_ids(&v).contains(&a));
    let b = v.new_raster_layer("b").unwrap();
    assert_ne!(a, b, "undo then create mints a fresh id");

    // (5) The package saved at the earlier revision: a layer created after reopening collides with no id it holds.
    let mut r = open(&early);
    let fresh = r.new_raster_layer("fresh").unwrap();
    assert!(!saved_ids.contains(&fresh), "{fresh} is held by the package: {saved_ids:?}");
    assert_eq!(filters(&mut r, &fresh)["filters"], json!([]));
    // (2) The id with revision bits is the same string after reopen and reaches the export that names it.
    assert!(layer_ids(&r).contains(&copy));
    let out = scratch.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let ran = row(&mut r, "export.run", json!({"dir": out.to_string_lossy(), "nodes": [copy]})).unwrap_or_else(|e| panic!("export.run refused: {} {}", e.code, e.detail));
    let files = ran["files"].as_array().unwrap_or_else(|| panic!("{ran}"));
    assert_eq!(files.len(), 1, "{ran}");
    assert_eq!(files[0]["node"], json!(copy), "{ran}");
    assert!(Path::new(files[0]["path"].as_str().unwrap()).is_file());
}

// ---- review fixes of d05d2699 (lead rulings 2026-11-13) ---------------------------------------------------------

fn revision(v: &Viewer) -> u64 {
    v.run().snapshot().unwrap().document().revision
}

/// The content bounds `effect.read` reports for `layer` as `(x, y, width, height)`.
fn content_bounds(v: &mut Viewer, layer: &str) -> (i64, i64, u64, u64) {
    let b = filters(v, layer)["content_bounds"].clone();
    (b["x"].as_i64().unwrap(), b["y"].as_i64().unwrap(), b["width"].as_u64().unwrap(), b["height"].as_u64().unwrap())
}

/// An identity liquify mesh over `(x, y, width, height)` at `spacing` (Folio's grid: `ceil((len - 1) / spacing) + 1`).
fn mesh(x: i64, y: i64, width: u64, height: u64, spacing: u64) -> Value {
    let grid = |len: u64| (len - 1).div_ceil(spacing) + 1;
    let (cols, rows) = (grid(width), grid(height));
    let n = usize::try_from(cols * rows).unwrap();
    json!({"origin_x": x, "origin_y": y, "width": width, "height": height, "spacing": spacing, "cols": cols, "rows": rows,
        "displacement": vec![[0.0, 0.0]; n], "freeze": vec![0.0; n]})
}

/// Review F1 (lead ruling Q1): an entry the render cannot run is refused at the command with the render's own
/// predicate (pigment `FilterKernel::admit` / `filter_window_cost` against `StackLimits::default()` over the layer's
/// content bounds), so no accepted entry leaves the frame blank. The review's examples: a box blur radius 2000
/// (12001 taps) and radius 127 with 50 iterations (12701 taps) against `MAX_BOX_KERNEL_TAPS` 6001.
#[test]
fn an_entry_the_render_cannot_run_is_refused_at_the_command() {
    let scratch = Scratch::new("raster-effects-budget");
    let src = scratch.0.join("art.psd");
    std::fs::write(&src, art_psd()).unwrap();
    let mut v = open(&src);
    let art = layer(&v, "art");
    let r = refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "box_blur", "params": {"radius_px": 2000}}), "effect_render_budget_exceeded");
    assert_eq!((r.detail["needed"].as_u64(), r.detail["limit"].as_u64()), (Some(12_001), Some(6_001)), "{}", r.detail);
    let r = refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "box_blur", "params": {"radius_px": 127, "iterations": 50}}), "effect_render_budget_exceeded");
    assert_eq!((r.detail["needed"].as_u64(), r.detail["limit"].as_u64()), (Some(12_701), Some(6_001)), "{}", r.detail);
    let r = refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "surface_blur", "params": {"radius_px": 4_294_967_295u64, "threshold_levels": 10}}), "effect_render_budget_exceeded");
    assert!(r.detail["needed"].as_u64() > r.detail["limit"].as_u64(), "{}", r.detail);
    // At the bound the entry is taken and renders (6001 taps).
    let plain = frame(&mut v);
    ok(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "box_blur", "params": {"radius_px": 1000, "iterations": 3}}));
    let wide = frame(&mut v);
    assert_ne!(wide, plain, "the accepted entry renders");
    // An update into the budget refusal is refused the same way and keeps the entry.
    refused(&mut v, "effect.update", json!({"layer": art, "instance": "fx-1", "params": {"radius_px": 1001}}), "effect_render_budget_exceeded");
    assert_eq!(frame(&mut v), wide);
    // A liquify mesh renders over the layer's content bounds: another mesh is refused naming them.
    let (x, y, w, h) = content_bounds(&mut v, &art);
    let r = refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "liquify", "params": {"mesh": mesh(x + 1, y, w, h, 8), "resample": "bilinear"}}), "parameter_invalid");
    assert_eq!(r.detail["key"], "params.mesh", "{}", r.detail);
    ok(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "liquify", "params": {"mesh": mesh(x, y, w, h, 8), "resample": "bilinear"}}));
    assert_eq!(filters(&mut v, &art)["filters"][1]["filter_kind"], "liquify");
    frame(&mut v);
}

/// Review F8: the hard bound is checked on the 32-bit value the record stores: a range sigma that rounds to 0 is
/// the 0 the engine refuses, refused here as out of range (exclusive minimum), not stored.
#[test]
fn a_float_parameter_is_checked_on_the_value_the_record_stores() {
    let scratch = Scratch::new("raster-effects-f32");
    let src = scratch.0.join("art.psd");
    std::fs::write(&src, art_psd()).unwrap();
    let mut v = open(&src);
    let art = layer(&v, "art");
    let r = refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "bilateral_blur", "params": {"radius_px": 1, "range_sigma": 1e-46}}), "effect_param_out_of_range");
    assert_eq!((r.detail["param"].as_str(), r.detail["hard_min_exclusive"].as_bool()), (Some("range_sigma"), Some(true)), "{}", r.detail);
    refused(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "gaussian_blur", "params": {"radius_px": 1e300}}), "parameter_invalid");
    let taken = ok(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "bilateral_blur", "params": {"radius_px": 1, "range_sigma": 1e-30}}));
    assert_eq!(taken["instance_id"], "fx-1");
    let stored = filters(&mut v, &art)["filters"][0]["params"]["range_sigma"].as_f64().unwrap();
    assert!(stored > 0.0 && stored as f32 == 1e-30_f32, "the record holds the 32-bit value: {stored}");
}

/// Review F10: the radial blur centre defaults to the centre of the layer's content bounds (the table's 50 %, 50 %);
/// the dust threshold is the engine's 0..1 channel difference, the table's `pixel` kept as `table_unit`.
#[test]
fn the_radial_blur_centre_defaults_to_the_layer_centre_and_the_dust_threshold_is_normalized() {
    let scratch = Scratch::new("raster-effects-defaults");
    let src = scratch.0.join("art.psd");
    std::fs::write(&src, art_psd()).unwrap();
    let mut v = open(&src);
    let art = layer(&v, "art");
    let (x, y, w, h) = content_bounds(&mut v, &art);
    let a = ok(&mut v, "effect.apply", json!({"layer": art, "filter_kind": "radial_blur"}));
    assert_eq!(a["defaulted"], json!(["centre_x_px", "centre_y_px", "amount", "kind"]), "{a}");
    let params = filters(&mut v, &art)["filters"][0]["params"].clone();
    let centre = (x as f64 + w as f64 * 0.5, y as f64 + h as f64 * 0.5);
    assert_eq!((params["centre_x_px"].as_f64(), params["centre_y_px"].as_f64()), (Some(centre.0), Some(centre.1)), "{params}");
    let specs = ok(&mut v, "effect.parameters", json!({"filter_kind": "dust_and_scratches"}));
    let threshold = specs["kinds"][0]["parameters"].as_array().unwrap().iter().find(|p| p["param_key"] == "threshold").unwrap().clone();
    assert_eq!((threshold["unit"].as_str(), threshold["table_unit"].as_str(), threshold["hard_max"].as_f64()), (Some("normalized_0_1"), Some("pixel"), Some(1.0)), "{threshold}");
}

/// Review F5 (option b): the effect edits and `adjustment.edit` are entry-only steps (one undo step, the document
/// revision unchanged); `adjustment.create` and `adjustment.apply` are revisions.
#[test]
fn effect_and_adjustment_edits_are_entry_only_steps_and_leave_the_revision() {
    let scratch = Scratch::new("raster-effects-revision");
    let src = scratch.0.join("solo.psd");
    std::fs::write(&src, solo_psd()).unwrap();
    let mut v = open(&src);
    let solo = layer(&v, "solo");
    let at = revision(&v);
    ok(&mut v, "effect.apply", json!({"layer": solo, "filter_kind": "gaussian_blur", "params": {"radius_px": 1}}));
    ok(&mut v, "effect.update", json!({"layer": solo, "instance": "fx-1", "opacity": 40}));
    ok(&mut v, "effect.toggle", json!({"layer": solo, "instance": "fx-1", "enabled": false}));
    ok(&mut v, "effect.apply", json!({"layer": solo, "filter_kind": "sharpen"}));
    ok(&mut v, "effect.reorder", json!({"layer": solo, "instance": "fx-2", "to": 0}));
    ok(&mut v, "effect.remove", json!({"layer": solo, "instance": "fx-2"}));
    assert_eq!(revision(&v), at, "the effect edits leave the revision");
    let adj = ok(&mut v, "adjustment.create", json!({"adjustment": {"kind": "invert"}}))["layer_id"].as_str().unwrap().to_string();
    let created = revision(&v);
    assert!(created > at, "adjustment.create is a revision: {at} -> {created}");
    ok(&mut v, "adjustment.edit", json!({"layer": adj, "adjustment": {"kind": "posterize", "levels": 4}}));
    assert_eq!(revision(&v), created, "adjustment.edit leaves the revision");
    v.select(Some(solo.clone()));
    ok(&mut v, "adjustment.apply", json!({"layer": solo, "adjustment": {"kind": "invert"}}));
    assert!(revision(&v) > created, "adjustment.apply is a revision");
}

/// Review F9: the vertex lists are capped at Folio's node budget of the raster records (4096: a 256 x 256 mesh at
/// spacing 1, 65536 vertices, is refused `budget_exceeded` before decoding); `effect.read` summarises a liquify
/// mesh unless `full`; an opacity-only or resample-only update keeps the stored mesh (no decode of it again).
#[test]
fn a_large_liquify_mesh_is_summarised_and_kept_by_updates() {
    let scratch = Scratch::new("raster-effects-mesh");
    let src = scratch.0.join("big.psd");
    let n = 256 * 256;
    std::fs::write(&src, layered_psd(256, 256, &[L { name: "big", rect: [0, 0, 256, 256], channels: vec![(0, vec![90; n]), (1, vec![120; n]), (2, vec![150; n])] }])).unwrap();
    let mut v = open(&src);
    let big = layer(&v, "big");
    let (x, y, w, h) = content_bounds(&mut v, &big);
    assert_eq!((w, h), (256, 256));
    let r = refused(&mut v, "effect.apply", json!({"layer": big, "filter_kind": "liquify", "params": {"mesh": mesh(x, y, w, h, 1), "resample": "bilinear"}}), "budget_exceeded");
    assert_eq!((r.detail["key"].as_str(), r.detail["at_most"].as_u64()), (Some("params.mesh.displacement"), Some(4096)), "{}", r.detail);
    // Spacing 5: 52 x 52 = 2704 vertices, under the cap.
    let mut m = mesh(x, y, w, h, 5);
    m["displacement"][1000] = json!([1.5, -0.5]);
    ok(&mut v, "effect.apply", json!({"layer": big, "filter_kind": "liquify", "params": {"mesh": m, "resample": "bilinear"}}));
    let summary = filters(&mut v, &big);
    let s = &summary["filters"][0]["params"]["mesh"];
    assert_eq!((s["summary"].as_bool(), s["vertices"].as_u64(), s["cols"].as_u64(), s["rows"].as_u64()), (Some(true), Some(2_704), Some(52), Some(52)), "{s}");
    assert!(summary.to_string().len() < 2_000, "the summary is small: {} bytes", summary.to_string().len());
    let full = |v: &mut Viewer| ok(v, "effect.read", json!({"layer": big, "full": true}))["filters"][0]["params"]["mesh"].clone();
    let before = full(&mut v);
    assert_eq!(before["displacement"].as_array().unwrap().len(), 2_704);
    assert_eq!(before["displacement"][1000], json!([1.5, -0.5]));
    ok(&mut v, "effect.update", json!({"layer": big, "instance": "fx-1", "opacity": 60}));
    assert_eq!(full(&mut v), before, "an opacity update keeps the mesh");
    ok(&mut v, "effect.update", json!({"layer": big, "instance": "fx-1", "params": {"resample": "nearest_neighbour"}}));
    assert_eq!(full(&mut v), before, "a resample update keeps the mesh");
    assert_eq!(filters(&mut v, &big)["filters"][0]["params"]["resample"], "nearest_neighbour");
    refused(&mut v, "effect.update", json!({"layer": big, "instance": "fx-1", "params": {"radius_px": 1}}), "parameter_unknown");
}

/// Job port double: refuses every admit, counts the asks (as `r60_export_routes`).
#[derive(Default)]
struct Saturated {
    asks: std::sync::atomic::AtomicU64,
}
impl hsk_studio_session::JobPort for Saturated {
    fn admit(&self, _scope: &hsk_studio_session::JobScope<'_>, _cost: hsk_studio_session::Cost) -> Result<hsk_studio_session::JobTicket, hsk_studio_session::JobRefused> {
        self.asks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(hsk_studio_session::JobRefused { retry_after_ms: 25 })
    }
    fn release(&self, _ticket: hsk_studio_session::JobTicket) {}
}

/// Review Q5: `adjustment.apply` is one job admitted through the viewer's bound JobPort; a saturated port refuses it
/// `overloaded`, is asked exactly once, and no pixel, step or entry changes.
#[test]
fn adjustment_apply_is_admitted_through_the_bound_job_port() {
    let scratch = Scratch::new("raster-effects-jobs");
    let src = scratch.0.join("solo.psd");
    std::fs::write(&src, solo_psd()).unwrap();
    let mut v = open(&src);
    let solo = layer(&v, "solo");
    let plain = frame(&mut v);
    let port = std::sync::Arc::new(Saturated::default());
    v.set_job_port(port.clone());
    let r = refused(&mut v, "adjustment.apply", json!({"layer": solo, "adjustment": {"kind": "invert"}}), "overloaded");
    assert_eq!(r.detail["retry_after_ms"], 25, "{}", r.detail);
    assert_eq!(port.asks.load(std::sync::atomic::Ordering::SeqCst), 1, "asked exactly once");
    assert_eq!(frame(&mut v), plain, "no pixel changed");
    // Refusals before admission do not ask the port.
    refused(&mut v, "adjustment.apply", json!({"layer": solo, "adjustment": {"kind": "ocio"}}), "parameter_invalid");
    assert_eq!(port.asks.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// Review F2: the adjustment (`7ede-9300`) and placed (`7ede-9200`) mints never mint an id again: create, delete,
/// create gives another id, in the session and after save and reopen, so no new layer takes a deleted one's
/// `raster_records` row.
#[test]
fn adjustment_and_placed_layer_ids_are_never_minted_twice() {
    let scratch = Scratch::new("raster-effects-mint-blocks");
    let src = scratch.0.join("art.psd");
    std::fs::write(&src, art_psd()).unwrap();
    let mut v = open(&src);
    let create = |v: &mut Viewer| ok(v, "adjustment.create", json!({"adjustment": {"kind": "invert"}}))["layer_id"].as_str().unwrap().to_string();
    let first = create(&mut v);
    assert!(first.contains("-7ede-9300-"), "{first}");
    v.delete_layer(&first).unwrap();
    let second = create(&mut v);
    assert_ne!(second, first, "a deleted adjustment layer's id is not minted again");
    let place = |v: &mut Viewer| {
        let placed = v.place_asset(&src, PlacementKind::Embedded, &PlaceOptions::default()).unwrap_or_else(|e| panic!("{e}"));
        placed["layer_id"].as_str().unwrap().to_string()
    };
    let p1 = place(&mut v);
    assert!(p1.contains("-7ede-9200-"), "{p1}");
    v.delete_layer(&p1).unwrap();
    let p2 = place(&mut v);
    assert_ne!(p2, p1, "a deleted placed layer's id is not minted again");
    v.delete_layer(&second).unwrap();
    let saved = scratch.0.join("blocks.handshake");
    v.save(&saved, false).unwrap();
    let held: Vec<String> = layer_ids(&v).into_iter().chain(record_ids(&v)).chain([first, second, p1]).collect();
    let mut r = open(&saved);
    let third = create(&mut r);
    let p3 = place(&mut r);
    assert!(!held.contains(&third) && !held.contains(&p3), "{third} {p3} vs {held:?}");
    assert_eq!(ok(&mut r, "effect.read", json!({"layer": p3}))["filters"], json!([]));
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
    assert_eq!(s.close(), 1, "the session refused a row (effect.update), so it ends with exit 1");
}
