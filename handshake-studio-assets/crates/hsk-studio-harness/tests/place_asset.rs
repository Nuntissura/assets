//! Lane SMB-PLACE-ASSET-1 (feature `viewer`): place embedded or linked assets into a raster document, raster
//! Move and Free transform (`view::place`, typed rows `asset.*` and `transform.*`). Scenario steps:
//! workflows/scenario-photo-retouch.json 83 and 86, scenario-vector-brand.json 54, scenario-board-slides-web.json
//! 64. Headless, render-cpu, files under TMP; one test drives the real process over JSONL.
#![cfg(feature = "viewer")]

use hsk_studio_harness::cli::{self, WindowArgs};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::place::{Anchor, FreeTransform, PlaceOptions, PlacementKind};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};
use hsk_studio_interop_raster as raster;
use hsk_studio_pigment::transform_math::ResampleFilter;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hsk-place-asset-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const PLATE: [u8; 3] = [40, 90, 200];
const HERO: [u8; 3] = [220, 60, 30];
const HERO_2: [u8; 3] = [30, 200, 60];

/// A solid opaque RGB PNG.
fn png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    let samples = raster::Samples::U8((0..width * height).flat_map(|_| rgb).collect());
    raster::encode_png(&raster::PngEncodeRequest { width, height, colour: raster::Colour::Rgb, samples: &samples, icc: None, compression: raster::PngCompression::Fast }).unwrap()
}

fn write(path: &Path, bytes: &[u8]) -> PathBuf {
    std::fs::write(path, bytes).unwrap();
    path.to_path_buf()
}

/// A 48 x 32 plate opened as the document, saved as a package in `s` and reopened from there (so links
/// resolve against `s`).
fn plate(s: &Scratch) -> Viewer {
    let source = write(&s.path("plate.png"), &png(48, 32, PLATE));
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap_or_else(|e| panic!("{e}"));
    let package = s.path("plate.handshake");
    v.save(&package, false).unwrap_or_else(|e| panic!("{e}"));
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&package).unwrap_or_else(|e| panic!("{e}"));
    v
}

fn px(v: &mut Viewer, x: i64, y: i64) -> [u8; 4] {
    v.render_if_dirty();
    let f = v.frame().expect("a frame");
    let i = usize::try_from(((y - f.y) * i64::from(f.width) + (x - f.x)) * 4).unwrap();
    [f.rgba8[i], f.rgba8[i + 1], f.rgba8[i + 2], f.rgba8[i + 3]]
}

fn near(a: [u8; 4], rgb: [u8; 3]) -> bool {
    a[3] == 255 && a[..3].iter().zip(rgb).all(|(x, y)| x.abs_diff(y) <= 2)
}

fn placements(v: &mut Viewer) -> Vec<Value> {
    v.list_placements()["placements"].as_array().cloned().unwrap_or_default()
}

fn layers(v: &Viewer) -> usize {
    v.run().snapshot().unwrap().document().layers.len()
}

fn last_undo(v: &Viewer) -> String {
    v.run().history()["undo"].as_array().and_then(|u| u.last()).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn embedded(v: &mut Viewer, s: &Scratch, name: &str, rgb: [u8; 3], x: f64, y: f64) -> Value {
    let hero = write(&s.path(name), &png(16, 8, rgb));
    v.place_asset(&hero, PlacementKind::Embedded, &PlaceOptions { x: Some(x), y: Some(y), ..PlaceOptions::default() }).unwrap_or_else(|e| panic!("{e}"))
}

/// Steps photo-retouch 83 (embedded variant) and vector-brand 54: a second picture placed into the document is a
/// placed-asset layer, one named undo step, and it survives save and reopen with its source, so it transforms
/// again from the source after the reopen.
#[test]
fn an_embedded_placement_is_one_step_and_survives_save_and_reopen() {
    let s = Scratch::new("embedded");
    let mut v = plate(&s);
    let before = layers(&v);
    let placed = embedded(&mut v, &s, "hero.png", HERO, 4.0, 6.0);
    let layer = placed["layer_id"].as_str().unwrap().to_owned();
    assert_eq!((placed["placement_kind"].as_str(), placed["resample"].as_str(), placed["scale_pct"].as_f64()), (Some("embedded"), Some("bicubic"), Some(100.0)), "{placed}");
    assert_eq!(layers(&v), before + 1);
    assert!(last_undo(&v).starts_with("asset.place"), "{}", last_undo(&v));
    assert!(near(px(&mut v, 5, 7), HERO) && near(px(&mut v, 19, 13), HERO), "{:?}", px(&mut v, 5, 7));
    assert!(near(px(&mut v, 2, 2), PLATE) && near(px(&mut v, 21, 7), PLATE), "{:?}", px(&mut v, 21, 7));
    let rows = placements(&mut v);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0]["layer_id"].as_str(), rows[0]["link_state"].as_str()), (Some(layer.as_str()), Some("embedded")));
    assert_eq!(rows[0]["transform"], json!([1.0, 0.0, 0.0, 1.0, 4.0, 6.0]));
    assert_eq!(v.state_json()["placed"]["placements"].as_array().map(Vec::len), Some(1));
    // Undoing the placement removes the layer and its record together; redo brings both back.
    v.undo().unwrap();
    assert_eq!((layers(&v), placements(&mut v).len()), (before, 0));
    assert!(near(px(&mut v, 5, 7), PLATE));
    v.redo().unwrap();
    assert_eq!((layers(&v), placements(&mut v).len()), (before + 1, 1));

    let out = s.path("out.handshake");
    v.save(&out, false).unwrap_or_else(|e| panic!("{e}"));
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&out).unwrap_or_else(|e| panic!("{e}"));
    let rows = placements(&mut w);
    assert_eq!((rows.len(), rows[0]["placement_kind"].as_str()), (1, Some("embedded")), "{rows:?}");
    assert!(near(px(&mut w, 5, 7), HERO));
    // The source came back with the package: a transform renders from it.
    let t = FreeTransform { scale_x: 200.0, scale_y: 200.0, reference_point: Anchor::TopLeft, ..FreeTransform::default() };
    let done = w.free_transform(&layer, &t).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(done["transform"], json!([2.0, 0.0, 0.0, 2.0, 4.0, 6.0]), "{done}");
    assert!(near(px(&mut w, 33, 20), HERO), "{:?}", px(&mut w, 33, 20));
    assert!(last_undo(&w).starts_with("transform.free_transform"), "{}", last_undo(&w));
    w.undo().unwrap();
    assert_eq!(placements(&mut w)[0]["transform"], json!([1.0, 0.0, 0.0, 1.0, 4.0, 6.0]));
    assert!(near(px(&mut w, 33, 20), PLATE));
}

/// Steps photo-retouch 83 and board-slides-web 64: a linked placement records the file relative to the document's
/// folder; its link state follows the file, Update All Links renders the new bytes in one step, and a file outside
/// the folder is refused.
#[test]
fn a_linked_placement_follows_its_file_and_updates_in_one_step() {
    let s = Scratch::new("linked");
    let mut v = plate(&s);
    let stall = write(&s.path("stall.png"), &png(16, 8, HERO));
    let placed = v.place_asset(&stall, PlacementKind::Linked, &PlaceOptions { x: Some(0.0), y: Some(0.0), ..PlaceOptions::default() }).unwrap_or_else(|e| panic!("{e}"));
    let layer = placed["layer_id"].as_str().unwrap().to_owned();
    assert_eq!(placed["uri"], "stall.png", "{placed}");
    assert_eq!(placements(&mut v)[0]["link_state"], "resolved");
    assert!(near(px(&mut v, 3, 3), HERO));

    write(&stall, &png(16, 8, HERO_2));
    assert_eq!(placements(&mut v)[0]["link_state"], "modified");
    let refused = v.move_content(&layer, 2.0, 0.0).unwrap_err();
    assert_eq!(refused.code, "placed_link_modified", "{refused}");
    let steps = v.run().history()["undo"].as_array().map_or(0, Vec::len);
    let updated = v.update_all_modified().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((updated["updated"].as_array().map(Vec::len), updated["changed"].as_bool()), (Some(1), Some(true)), "{updated}");
    assert_eq!(v.run().history()["undo"].as_array().map_or(0, Vec::len), steps + 1);
    assert!(last_undo(&v).starts_with("asset.update_all_modified"), "{}", last_undo(&v));
    assert!(near(px(&mut v, 3, 3), HERO_2), "{:?}", px(&mut v, 3, 3));
    assert_eq!(placements(&mut v)[0]["link_state"], "resolved");
    // Undo brings the old pixels and the old digest back.
    v.undo().unwrap();
    assert!(near(px(&mut v, 3, 3), HERO));
    v.redo().unwrap();

    std::fs::remove_file(&stall).unwrap();
    assert_eq!(placements(&mut v)[0]["link_state"], "missing");
    let missing = v.update_all_modified().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(missing["missing"].as_array().map(Vec::len), Some(1), "{missing}");
    assert_eq!(placements(&mut v)[0]["health"], "missing");
    assert_eq!(v.move_content(&layer, 1.0, 0.0).unwrap_err().code, "placed_link_missing");

    let outside = Scratch::new("linked-outside");
    let far = write(&outside.path("far.png"), &png(4, 4, HERO));
    assert_eq!(v.place_asset(&far, PlacementKind::Linked, &PlaceOptions::default()).unwrap_err().code, "link_outside_document_folder");
    // Embed Link: a resolved link becomes embedded and keeps its pixels.
    let again = write(&s.path("again.png"), &png(8, 8, HERO_2));
    let second = v.place_asset(&again, PlacementKind::Linked, &PlaceOptions { x: Some(30.0), y: Some(20.0), ..PlaceOptions::default() }).unwrap_or_else(|e| panic!("{e}"));
    let second_layer = second["layer_id"].as_str().unwrap();
    v.embed_link(second_layer).unwrap_or_else(|e| panic!("{e}"));
    std::fs::remove_file(&again).unwrap();
    let row = placements(&mut v).into_iter().find(|r| r["layer_id"] == second_layer).unwrap();
    assert_eq!((row["placement_kind"].as_str(), row["link_state"].as_str()), (Some("embedded"), Some("embedded")), "{row}");
    v.move_content(second_layer, 2.0, 1.0).unwrap_or_else(|e| panic!("{e}"));
    assert!(near(px(&mut v, 33, 22), HERO_2));
}

/// Step photo-retouch 86: Free Transform and Move on a placed layer accumulate on the record and render from the
/// source; Reset Transform returns to the placement; on a pixel layer Move is an exact whole-pixel copy.
#[test]
fn free_transform_move_and_reset_on_placed_and_pixel_layers() {
    let s = Scratch::new("transform");
    let mut v = plate(&s);
    let placed = embedded(&mut v, &s, "hero.png", HERO, 8.0, 8.0);
    let layer = placed["layer_id"].as_str().unwrap().to_owned();
    let half = FreeTransform { scale_x: 50.0, scale_y: 50.0, reference_point: Anchor::TopLeft, ..FreeTransform::default() };
    assert_eq!(v.free_transform(&layer, &half).unwrap()["transform"], json!([0.5, 0.0, 0.0, 0.5, 8.0, 8.0]));
    assert!(near(px(&mut v, 9, 9), HERO) && near(px(&mut v, 20, 9), PLATE), "{:?}", px(&mut v, 20, 9));
    let moved = v.move_content(&layer, 2.5, 4.0).unwrap();
    assert_eq!(moved["transform"], json!([0.5, 0.0, 0.0, 0.5, 10.5, 12.0]), "{moved}");
    let turned = FreeTransform { angle: 90.0, ..FreeTransform::default() };
    let t = v.free_transform(&layer, &turned).unwrap()["transform"].clone();
    let m: Vec<f64> = t.as_array().unwrap().iter().map(|n| n.as_f64().unwrap()).collect();
    assert!((m[0]).abs() < 1e-9 && (m[1] - 0.5).abs() < 1e-9 && (m[2] + 0.5).abs() < 1e-9, "{t}");
    let reset = v.reset_transform(&layer).unwrap();
    assert_eq!(reset["transform"], json!([1.0, 0.0, 0.0, 1.0, 8.0, 8.0]), "{reset}");
    assert!(near(px(&mut v, 23, 15), HERO));

    // The plate layer is pixels: a move is exact and whole-pixel only.
    let plate_layer = v.layers().iter().find(|r| r.layer_id != layer).map(|r| r.layer_id.clone()).unwrap();
    v.set_visible(&layer, false).unwrap();
    assert_eq!(v.move_content(&plate_layer, 1.5, 0.0).unwrap_err().code, "move_not_whole_pixels");
    let moved = v.move_content(&plate_layer, 10.0, 0.0).unwrap();
    assert_eq!(moved["placed"], false, "{moved}");
    assert_eq!(px(&mut v, 5, 5)[3], 0, "the vacated column is transparent");
    assert!(near(px(&mut v, 12, 5), PLATE) && near(px(&mut v, 47, 31), PLATE));
    assert_eq!(v.reset_transform(&plate_layer).unwrap_err().code, "layer_not_placed");
    let shrunk = v.free_transform(&plate_layer, &FreeTransform { scale_x: 50.0, scale_y: 50.0, ..FreeTransform::default() }).unwrap();
    assert!(shrunk["tiles"].as_u64().unwrap() >= 1, "{shrunk}");
    // Review F2 (9d83222c): the reference point is on the layer's pixels (x 10..58, y 0..32 after the move), not
    // on its 256-pixel tile grid: 50 % about middle_centre lands on x 22..46, y 8..24.
    for (x, y) in [(24, 10), (44, 22), (34, 16)] {
        assert!(near(px(&mut v, x, y), PLATE), "({x}, {y}) {:?}", px(&mut v, x, y));
    }
    for (x, y) in [(2, 2), (19, 16), (34, 5), (34, 27)] {
        assert_eq!(px(&mut v, x, y)[3], 0, "({x}, {y}) lies outside the shrunk plate");
    }
    v.undo().unwrap();
    v.undo().unwrap();
    assert!(near(px(&mut v, 5, 5), PLATE), "{:?}", px(&mut v, 5, 5));
}

/// Hostile and wrong input: every refusal is typed and changes nothing.
#[test]
fn hostile_input_is_refused_typed_and_changes_nothing() {
    let s = Scratch::new("hostile");
    let mut v = plate(&s);
    let revision = v.run().snapshot().unwrap().document().revision;
    let none = PlaceOptions::default();
    assert_eq!(v.place_asset(&s.path("absent.png"), PlacementKind::Embedded, &none).unwrap_err().code, "resource_unreadable");
    let junk = write(&s.path("junk.png"), b"not an image at all");
    assert_eq!(v.place_asset(&junk, PlacementKind::Embedded, &none).unwrap_err().code, "format_unrecognised");
    let hero = write(&s.path("hero.png"), &png(16, 8, HERO));
    for bad in [PlaceOptions { scale_pct: Some(0.0), ..PlaceOptions::default() }, PlaceOptions { scale_pct: Some(1.0e9), ..PlaceOptions::default() }, PlaceOptions { opacity_pct: Some(101.0), ..PlaceOptions::default() }, PlaceOptions { x: Some(f64::NAN), ..PlaceOptions::default() }] {
        assert_eq!(v.place_asset(&hero, PlacementKind::Embedded, &bad).unwrap_err().code, "parameter_out_of_range", "{bad:?}");
    }
    assert_eq!(v.run().snapshot().unwrap().document().revision, revision);
    let placed = embedded(&mut v, &s, "hero2.png", HERO, 0.0, 0.0);
    let layer = placed["layer_id"].as_str().unwrap().to_owned();
    let revision = v.run().snapshot().unwrap().document().revision;
    for (t, code) in [
        (FreeTransform { angle: 400.0, ..FreeTransform::default() }, "parameter_out_of_range"),
        (FreeTransform { skew_x: 90.0, ..FreeTransform::default() }, "parameter_out_of_range"),
        (FreeTransform { scale_x: 0.0, ..FreeTransform::default() }, "transform_singular"),
        (FreeTransform { scale_x: 1.0e12, ..FreeTransform::default() }, "transform_scale_out_of_range"),
        (FreeTransform { translate_x: f64::INFINITY, ..FreeTransform::default() }, "parameter_out_of_range"),
    ] {
        assert_eq!(v.free_transform(&layer, &t).unwrap_err().code, code, "{t:?}");
    }
    assert_eq!(v.move_content("SLYR-019abcde-0000-7000-8000-00000000dead", 1.0, 1.0).unwrap_err().code, "layer_unknown");
    assert_eq!(v.embed_link(&layer).unwrap_err().code, "layer_not_linked");
    // Paint would be lost at the next render from the source: the placed layer refuses it, as the fill tools do.
    let stroke = [hsk_studio_pigment::brush::StrokeSample { x: 2.0, y: 2.0, pressure: None, time_ms: None }];
    assert!(v.paint(&layer, &stroke).unwrap_err().contains("layer_not_pixel"));
    assert_eq!(v.run().snapshot().unwrap().document().revision, revision);
    // A blank document opened from no file has no folder to link against; embedding needs none.
    let mut fresh = Viewer::new(Prefer::Cpu);
    let spec = NewDocument::new(DocumentProfile::Raster, Some((32.0, 32.0)));
    fresh.new_document(&spec, None).unwrap();
    assert_eq!(fresh.place_asset(&hero, PlacementKind::Linked, &none).unwrap_err().code, "link_base_unknown");
    let ok = fresh.place_asset(&hero, PlacementKind::Embedded, &none).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(ok["placement_kind"], "embedded");
}

// ----- review fixes of 9d83222c (research/review-smb-place-asset-1-9d83222c.json) -----------------

/// One PSD layer: name, `[top, left, bottom, right]`, channels by id, additional-info blocks.
struct PsdLayer {
    name: &'static str,
    rect: [i32; 4],
    channels: Vec<(i16, Vec<u8>)>,
    blocks: Vec<([u8; 4], Vec<u8>)>,
}

/// An RGB PSD of `depth` bits (8 or 32), raw channels, no masks; `merged` is the composite image data.
fn psd(width: u32, height: u32, depth: u16, layers: &[PsdLayer], merged: &[u8]) -> Vec<u8> {
    let mut v = b"8BPS".to_vec();
    v.extend(1u16.to_be_bytes());
    v.extend([0u8; 6]);
    v.extend(3u16.to_be_bytes());
    v.extend(height.to_be_bytes());
    v.extend(width.to_be_bytes());
    v.extend(depth.to_be_bytes());
    v.extend(3u16.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    let mut info = i16::try_from(layers.len()).unwrap().to_be_bytes().to_vec();
    for l in layers {
        for r in l.rect {
            info.extend(r.to_be_bytes());
        }
        info.extend(u16::try_from(l.channels.len()).unwrap().to_be_bytes());
        for (id, data) in &l.channels {
            info.extend(id.to_be_bytes());
            info.extend(u32::try_from(2 + data.len()).unwrap().to_be_bytes());
        }
        info.extend_from_slice(b"8BIMnorm");
        info.extend([255u8, 0, 0x08, 0]);
        let mut extra = 0u32.to_be_bytes().to_vec();
        extra.extend(0u32.to_be_bytes());
        let mut pascal = vec![u8::try_from(l.name.len()).unwrap()];
        pascal.extend_from_slice(l.name.as_bytes());
        while pascal.len() % 4 != 0 {
            pascal.push(0);
        }
        extra.extend(pascal);
        for (key, data) in &l.blocks {
            extra.extend_from_slice(b"8BIM");
            extra.extend_from_slice(key);
            extra.extend(u32::try_from(data.len()).unwrap().to_be_bytes());
            extra.extend(data);
        }
        info.extend(u32::try_from(extra.len()).unwrap().to_be_bytes());
        info.extend(extra);
    }
    for l in layers {
        for (_, data) in &l.channels {
            info.extend(0u16.to_be_bytes());
            info.extend_from_slice(data);
        }
    }
    if info.len() % 2 == 1 {
        info.push(0);
    }
    let mut section = u32::try_from(info.len()).unwrap().to_be_bytes().to_vec();
    section.extend(info);
    section.extend(0u32.to_be_bytes());
    v.extend(u32::try_from(section.len()).unwrap().to_be_bytes());
    v.extend(section);
    v.extend(0u16.to_be_bytes());
    v.extend_from_slice(merged);
    v
}

const BASE: [u8; 3] = [40, 90, 200];

/// 64 x 48, 8-bit: a solid `base` layer under an Invert adjustment layer `inv` (a PSD import record the viewer
/// renders through composite's adapter).
fn inverted_psd() -> Vec<u8> {
    let (w, h) = (64u32, 48u32);
    let n = (w * h) as usize;
    let base = PsdLayer { name: "base", rect: [0, 0, h as i32, w as i32], channels: (0..3).map(|c| (c as i16, vec![BASE[c]; n])).collect(), blocks: vec![] };
    let inv = PsdLayer { name: "inv", rect: [0, 0, 0, 0], channels: vec![(0, vec![]), (1, vec![]), (2, vec![])], blocks: vec![(*b"nvrt", vec![])] };
    let merged: Vec<u8> = (0..3).flat_map(|c| vec![255 - BASE[c]; n]).collect();
    psd(w, h, 8, &[base, inv], &merged)
}

/// Linear-light values of the HDR layer: red above white.
const HDR: [f32; 3] = [1.5, 0.25, 0.5];

/// 64 x 64, 32-bit float (linear): one opaque layer `hdr` of [`HDR`] over the canvas.
fn hdr_psd() -> Vec<u8> {
    let w = 64u32;
    let n = (w * w) as usize;
    let plane = |v: f32| -> Vec<u8> { std::iter::repeat_n(v.to_be_bytes(), n).flatten().collect() };
    let channels = vec![(-1, plane(1.0)), (0, plane(HDR[0])), (1, plane(HDR[1])), (2, plane(HDR[2]))];
    let layer = PsdLayer { name: "hdr", rect: [0, 0, w as i32, w as i32], channels, blocks: vec![] };
    let merged: Vec<u8> = HDR.iter().flat_map(|c| plane(*c)).collect();
    psd(w, w, 32, &[layer], &merged)
}

fn layer_named(v: &Viewer, name: &str) -> String {
    v.layers().into_iter().find(|r| r.name == name).unwrap_or_else(|| panic!("no layer {name}: {}", v.state_json())).layer_id
}

/// Straight linear RGBA of document pixel `(x, y)` of raster layer `layer`, read from its tile bytes.
fn texel(v: &Viewer, layer: &str, x: i64, y: i64) -> [f32; 4] {
    let assets = v.run().render_assets().unwrap();
    let snap = v.run().snapshot().unwrap();
    let l = snap.document().layers.iter().find(|l| l.layer_id == layer).unwrap();
    let hsk_studio_folio::Payload::Raster { tiles } = &l.payload else { panic!("{layer} is not raster") };
    for t in tiles {
        let (tw, th) = (t.width.value as i64, t.height.value as i64);
        let (dx, dy) = (x - t.column * tw, y - t.row * th);
        if !(0..tw).contains(&dx) || !(0..th).contains(&dy) {
            continue;
        }
        let body = &assets[&t.content_digest.digest];
        let i = usize::try_from(dy * tw + dx).unwrap();
        let colour_len = usize::try_from(tw * th * 12).unwrap();
        let f = |k: usize| f32::from_le_bytes(body[k..k + 4].try_into().unwrap());
        return [f(i * 12), f(i * 12 + 4), f(i * 12 + 8), f(colour_len + i * 4)];
    }
    [0.0; 4]
}

/// Review F1: deleting a placed layer frees its id, and the next layer any other tool creates must not take that id
/// and inherit the placed record. After each kind of new layer, nothing is listed as placed, the raster layer
/// paints, a moved copy keeps its own pixels, the PSD's Invert layer (an import record) keeps rendering, and
/// asset.place still works, also after save and reopen.
#[test]
fn a_deleted_placement_leaves_no_record_on_the_next_new_layer() {
    let s = Scratch::new("delete");
    let source = write(&s.path("inverted.psd"), &inverted_psd());
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap_or_else(|e| panic!("{e}"));
    let base = layer_named(&v, "base");
    let inverted = px(&mut v, 2, 2);
    assert!(!near(inverted, BASE), "the Invert layer renders: {inverted:?}");
    let mut fresh = None;
    for kind in ["raster", "duplicate", "group", "text", "vector"] {
        let placed = embedded(&mut v, &s, &format!("hero-{kind}.png"), HERO, 40.0, 30.0);
        let placed_id = placed["layer_id"].as_str().unwrap().to_owned();
        v.delete_layer(&placed_id).unwrap();
        v.select(None);
        let created = match kind {
            "raster" => v.new_raster_layer("fresh").unwrap(),
            "duplicate" => v.duplicate_layer(&base).unwrap(),
            "group" => v.group_layers(&[fresh.clone().unwrap()], "group").unwrap(),
            "text" => v.place_text(30.0, 44.0, "Hi").unwrap(),
            _ => v.pencil_stroke(&[[36.0, 8.0], [60.0, 8.0]]).unwrap(),
        };
        assert!(placements(&mut v).is_empty(), "{kind}: {created} is not a placement: {:?}", placements(&mut v));
        assert_eq!(v.state_json()["placed"]["placements"].as_array().map(Vec::len), Some(0), "{kind}");
        match kind {
            "raster" => {
                let stroke = [hsk_studio_pigment::brush::StrokeSample { x: 50.0, y: 40.0, pressure: None, time_ms: None }];
                v.paint(&created, &stroke).unwrap_or_else(|e| panic!("raster: paint on a new layer: {e}"));
                fresh = Some(created.clone());
            }
            "duplicate" => {
                let moved = v.move_content(&created, 1.0, 0.0).unwrap_or_else(|e| panic!("{e}"));
                assert_eq!(moved["placed"], false, "{moved}");
            }
            _ => {}
        }
        // Undo and redo re-read the records: the Invert layer still renders, and a placement is accepted.
        v.undo().unwrap();
        v.redo().unwrap();
        assert_eq!(px(&mut v, 2, 2), inverted, "{kind}: the PSD Invert layer still renders");
    }
    let out = s.path("after-delete.handshake");
    v.save(&out, false).unwrap_or_else(|e| panic!("{e}"));
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&out).unwrap_or_else(|e| panic!("{e}"));
    assert!(placements(&mut w).is_empty(), "{:?}", placements(&mut w));
    assert_eq!(px(&mut w, 2, 2), inverted, "the Invert layer renders after the reopen");
    let again = embedded(&mut w, &s, "hero-again.png", HERO, 40.0, 30.0);
    assert_eq!(placements(&mut w).len(), 1, "{again}");
    assert!(near(px(&mut w, 41, 31), HERO), "{:?}", px(&mut w, 41, 31));
}

/// Review F4: Free Transform with a filter (bilinear, bicubic) on a float layer, and a placement of a float source,
/// keep linear values above 1.0 (the tiles are rgb_f32le; nothing integer is written).
#[test]
fn filtered_transforms_keep_float_highlights_above_one() {
    let s = Scratch::new("hdr");
    let source = write(&s.path("hdr.psd"), &hdr_psd());
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap_or_else(|e| panic!("{e}"));
    let layer = layer_named(&v, "hdr");
    assert!((texel(&v, &layer, 32, 32)[0] - HDR[0]).abs() < 1e-6, "imports as stored: {:?}", texel(&v, &layer, 32, 32));
    for filter in [ResampleFilter::Bilinear, ResampleFilter::Bicubic] {
        let t = FreeTransform { angle: 10.0, resample: Some(filter), ..FreeTransform::default() };
        v.free_transform(&layer, &t).unwrap_or_else(|e| panic!("{e}"));
        let c = texel(&v, &layer, 32, 32);
        assert!((c[0] - HDR[0]).abs() < 1e-3 && c[3] > 0.999, "{filter:?}: the highlight is kept: {c:?}");
        v.undo().unwrap();
    }
    // A placed float source, scaled (bicubic, the default): its instance tiles keep the highlight too.
    let placed = v.place_asset(&source, PlacementKind::Embedded, &PlaceOptions { x: Some(0.0), y: Some(0.0), scale_pct: Some(50.0), ..PlaceOptions::default() }).unwrap_or_else(|e| panic!("{e}"));
    let placed_layer = placed["layer_id"].as_str().unwrap();
    let c = texel(&v, placed_layer, 16, 16);
    assert!((c[0] - HDR[0]).abs() < 1e-3 && c[3] > 0.999, "the placed highlight is kept: {c:?}");
}

/// 16-bit PCM stereo WAV, 48 kHz, `frames` frames of a square wave.
#[cfg(feature = "media")]
fn wav(frames: u32) -> Vec<u8> {
    let (rate, data_len) = (48_000u32, frames * 4);
    let mut v = b"RIFF".to_vec();
    v.extend((36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend(16u32.to_le_bytes());
    v.extend(1u16.to_le_bytes());
    v.extend(2u16.to_le_bytes());
    v.extend(rate.to_le_bytes());
    v.extend((rate * 4).to_le_bytes());
    v.extend(4u16.to_le_bytes());
    v.extend(16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend(data_len.to_le_bytes());
    for i in 0..frames {
        let s: i16 = if (i / 64) % 2 == 0 { 8192 } else { -8192 };
        v.extend(s.to_le_bytes());
        v.extend(s.to_le_bytes());
    }
    v
}

/// Review F3: links resolve against the opened document's folder. Opening an audio file from another folder (the
/// document stays open) does not move that folder: the link stays resolved, and Update All Links does not read
/// the other folder's file of the same name.
#[cfg(feature = "media")]
#[test]
fn an_audio_file_opened_beside_the_document_does_not_move_its_link_folder() {
    let s = Scratch::new("link-base");
    let mut v = plate(&s);
    let stall = write(&s.path("stall.png"), &png(16, 8, HERO));
    v.place_asset(&stall, PlacementKind::Linked, &PlaceOptions { x: Some(0.0), y: Some(0.0), ..PlaceOptions::default() }).unwrap_or_else(|e| panic!("{e}"));
    let other = Scratch::new("link-base-audio");
    write(&other.path("stall.png"), &png(16, 8, HERO_2));
    let tone = write(&other.path("tone.wav"), &wav(4800));
    v.open(&tone).unwrap_or_else(|e| panic!("{e}"));
    assert!(v.audio().is_some(), "the audio file opened");
    assert_eq!(placements(&mut v)[0]["link_state"], "resolved", "{:?}", placements(&mut v));
    let updated = v.update_all_modified().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((updated["changed"].as_bool(), updated["updated"].as_array().map(Vec::len)), (Some(false), Some(0)), "{updated}");
    assert!(near(px(&mut v, 3, 3), HERO), "{:?}", px(&mut v, 3, 3));
}

// ----- the real process over JSONL --------------------------------------------------------------

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

/// One `command PACKAGE --stdin` session: every line in, every reply out.
fn session(package: &Path, lines: &[Value], dir: &Path) -> (i32, Vec<Value>) {
    let args: Vec<String> = ["command", package.to_str().unwrap(), "--actor", "lane-place", "--session", "s-place", "--stdin"].iter().map(|s| (*s).to_string()).collect();
    assert!(!matches!(cli::window_args(&args, &|p| p.is_file()), WindowArgs::Window { .. }), "{args:?} would open a window; not spawned");
    let mut command = Command::new(binary());
    command.args(&args).current_dir(dir).env("STUDIO_DIAG_DIR", dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().expect("spawn studio-harness");
    let mut stdin = child.stdin.take().unwrap();
    let input: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut out);
        out
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(120) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the session did not end");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    writer.join().unwrap();
    let out = reader.join().unwrap();
    let replies = out.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}"))).collect();
    (status.code().expect("an exit code"), replies)
}

fn line(id: &str, parameters: Value) -> Value {
    json!({"command": "command", "command_id": id, "parameters": parameters})
}

/// The scenario steps as a model runs them: place (embedded and linked), free transform, list, save and export
/// through `command --stdin`; each mutating line is one named undo step attributed to the caller, and the export
/// and the reopened package hold the placed pixels.
#[test]
fn a_model_places_transforms_saves_and_exports_through_the_process() {
    let s = Scratch::new("process");
    drop(plate(&s));
    let package = s.path("plate.handshake");
    let hero = write(&s.path("hero.png"), &png(16, 8, HERO));
    let stall = write(&s.path("stall.png"), &png(8, 8, HERO_2));
    let out = s.path("placed.handshake");
    let png_out = s.path("placed.png");
    let lines = [
        line("asset.place", json!({"path": hero.to_str().unwrap(), "placement_kind": "embedded", "x": 4, "y": 6})),
        line("transform.free_transform", json!({"layer": "$1", "scale_x": 200, "scale_y": 200, "reference_point": "top_left"})),
        line("asset.place", json!({"path": stall.to_str().unwrap(), "placement_kind": "linked", "x": 40, "y": 0, "opacity_pct": 30})),
        line("asset.list_placements", json!({})),
        line("asset.place", json!({"path": hero.to_str().unwrap(), "placement_kind": "sideways"})),
        line("transform.move", json!({"layer": "nobody", "dx": 1, "dy": 1})),
        line("document.save", json!({"path": out.to_str().unwrap()})),
        line("photo.export", json!({"path": png_out.to_str().unwrap(), "format": "png"})),
    ];
    // The layer id of line 1 is only known from its reply: run line 1 alone first.
    let (_, first) = session(&package, &lines[..1], &s.0);
    assert_eq!(first.len(), 3, "open, the line, ended: {first:?}");
    let layer_hint = first[1]["detail"]["result"]["layer_id"].as_str().expect("a layer id").to_owned();
    assert!(first[1]["detail"]["undo_name"].as_str().is_some_and(|n| n.starts_with("asset.place")), "{}", first[1]);
    assert_eq!(first[1]["attribution"]["actor"]["account_id"], "lane-place", "{}", first[1]["attribution"]);
    // The one-line session did not save: the package is unchanged, so the full run places again with the same id.
    let mut all = lines.to_vec();
    all[1]["parameters"]["layer"] = json!(layer_hint);
    let (exit, replies) = session(&package, &all, &s.0);
    assert_eq!(replies.len(), all.len() + 2, "{replies:?}");
    let reply = |n: usize| &replies[n];
    for n in [1, 2, 3, 4, 7, 8] {
        assert_eq!(reply(n)["ok"], true, "line {n}: {} {}", reply(n)["error"], reply(n)["detail"]);
    }
    assert_eq!(reply(1)["detail"]["result"]["layer_id"], layer_hint.as_str());
    assert!(reply(2)["detail"]["undo_name"].as_str().is_some_and(|n| n.starts_with("transform.free_transform")), "{}", reply(2));
    assert_eq!(reply(4)["detail"]["result"]["placements"].as_array().map(Vec::len), Some(2), "{}", reply(4));
    assert_eq!((reply(5)["error"].as_str(), reply(6)["error"].as_str()), (Some("parameter_invalid"), Some("layer_unknown")));
    assert_eq!(reply(4)["state"]["placed"]["placements"].as_array().map(Vec::len), Some(2));
    assert_eq!(exit, 1, "a refused line makes the exit 1");
    // The export holds the transformed placement and the dimmed link.
    let decoded = raster::decode(&std::fs::read(&png_out).unwrap(), &raster::Limits::default(), &hsk_studio_accord::CancellationToken::default()).unwrap();
    let raster::Samples::U8(samples) = &decoded.samples else { panic!("8-bit export") };
    let at = |x: usize, y: usize| {
        let c = decoded.colour.channels();
        let i = (y * decoded.width as usize + x) * c;
        [samples[i], samples[i + 1], samples[i + 2]]
    };
    assert!(at(33, 20).iter().zip(HERO).all(|(a, b)| a.abs_diff(b) <= 2), "{:?}", at(33, 20));
    assert!(at(44, 4).iter().zip(HERO_2).any(|(a, b)| a.abs_diff(b) > 20), "30 % opacity is not the full colour: {:?}", at(44, 4));
    let mut w = Viewer::new(Prefer::Cpu);
    w.open(&out).unwrap_or_else(|e| panic!("{e}"));
    let rows = placements(&mut w);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows.iter().any(|r| r["placement_kind"] == "linked" && r["link_state"] == "resolved" && r["uri"] == "stall.png"), "{rows:?}");
}
